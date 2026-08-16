//! Chat send/abort/providers. Ports `src/main/index.js`'s `chat:send`/
//! `chat:providers`/`chat:abort` handler BODIES (not `conductor.runChat`,
//! which those handlers call into for the real multi-turn work — see the
//! scope note below). OpenAI wire ports `streamOpenAI` SSE directly;
//! Anthropic goes through a hand-rolled `/v1/messages` SSE client on
//! `reqwest` rather than the SDK. Deltas stream via the event bus (see the
//! transport note below); abort delegates to `conductor::chat::abort_chat`,
//! which cancels the `CancellationToken` `conductor::Conductor` now holds
//! per chat id (see the tool-loop note below — this file no longer owns
//! that registry itself).
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
//! ## Tool loop: delegated to `conductor::chat::run_chat` (phase 5b)
//!
//! `index.js`'s real `chat:send` handler is a thin wrapper that resolves
//! the provider, builds `system` from `conductor.SYSTEM` (+ brain vault
//! context when `brainWs` is given), and hands off to `conductor.runChat`
//! — an 8-turn loop that streams text, executes tool calls against live
//! ptys (`chat:tool` events), and re-sends the transcript with tool
//! results appended. This file now mirrors that split exactly:
//! `chat_send` still owns provider resolution / key-missing handling /
//! betas-fallbacks (unchanged from before this phase), then builds a
//! `conductor::env::ConductorEnv` and calls `conductor::chat::run_chat` for
//! the actual multi-turn work — `run_chat` owns the abort registry, the
//! per-turn `tokio::select!` race, `TOOLS`, tool dispatch, and the
//! terminal `chat:done` for every internal exit path; this file's own
//! `catch`-equivalent (the `Err` arm below) only ever classifies a
//! genuine, non-abort stream failure (401/authy), exactly the split
//! `conductor::chat`'s own doc comment describes.
//!
//! Remaining, deliberate gap (unchanged from before this phase, still not
//! this file's or conductor's to close): `brain_ws`-driven vault context
//! (`brain.contextFor`) has no Rust port yet anywhere in this tree (grep
//! `brain.rs` — no `context_for`), so `system` here is `conductor`'s own
//! prompt alone, never `brain_ws`-extended. `brain_ws` stays accepted as a
//! parameter for wire-shape completeness.
//!
//! The renderer already resends its full transcript on every `chat.send`
//! call (`panels/chat.js`'s `tome.chat.send(this.chatId, this.history,
//! brainWs)`), so server-side history is never needed either way.

use std::collections::HashMap;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::chat::providers;
use crate::chat::sse;
use crate::conductor;
use crate::state::AppState;
use crate::{lock_gate, login_env, store};

/// One shared `reqwest::Client` for connection reuse across sends — a
/// module-local static rather than an `AppState` field, same rationale
/// `login_env.rs` uses for its own module-local cache. `reqwest::Client`
/// is `Clone + Send + Sync` and documented as cheap to share via a
/// long-lived reference; building a fresh one per `chat:send` would throw
/// connection pooling away for no benefit. `pub(crate)` — `conductor::env`'s
/// `production_env` is the tool loop's own caller of this, one turn at a
/// time.
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

pub(crate) fn http_client() -> &'static reqwest::Client {
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
    let (stored, custom_value) = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            (
                store::get(&dir, "chat-provider", locked),
                store::get(&dir, "custom-provider", locked),
            )
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let active = providers::active_provider_id(stored.as_str());
    let custom = providers::parse_custom_provider(&custom_value);

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
        .chain(std::iter::once(json!({
            "id": providers::CUSTOM_ID,
            "label": custom.as_ref().map(|c| c.label.as_str()).unwrap_or("Custom provider"),
            "model": custom.as_ref().map(|c| c.model.as_str()).unwrap_or(""),
            "keyEnv": Value::Null,
            "keySet": custom.is_some(),
        })))
        .collect();

    Ok(json!({ "providers": list, "active": active }))
}

/// Resolves the active provider + beta/fallback flags from the login shell
/// and stored chat-provider/chat-model keys. Mirrors `chat_send`'s inline
/// resolution, factored out so `chat_send`, `complete_once`, and the
/// `mentor_judge` command share one resolution path. Returns a friendly
/// `KeyMissing` error string when no key is available.
pub(crate) async fn resolve_chat(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<
    (
        providers::ResolvedProvider,
        Option<Vec<String>>,
        Option<String>,
    ),
    String,
> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let login = login_env::login_env().await;
    let env: HashMap<String, String> = std::env::vars().collect();
    let (stored_provider, stored_model, custom_value) = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            (
                store::get(&dir, "chat-provider", locked),
                store::get(&dir, "chat-model", locked),
                store::get(&dir, "custom-provider", locked),
            )
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let custom = providers::parse_custom_provider(&custom_value);

    let provider = match providers::resolve_chat_provider(
        &env,
        &login.secrets,
        stored_provider.as_str(),
        stored_model.as_str(),
        custom.as_ref(),
    ) {
        providers::ProviderResolution::KeyMissing { entry, .. } => {
            let message = if entry.id == providers::CUSTOM_ID {
                "Custom provider is not configured — set it up in Settings → Assistant.".to_string()
            } else {
                format!(
                    "{} needs {} — export it in your shell and restart, or pick another provider in \u{2318}, \u{2192} Assistant.",
                    entry.label, entry.key_env
                )
            };
            return Err(message);
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

    Ok((provider, betas, fallbacks))
}

/// Non-streaming one-shot completion: resolves the provider, streams into a
/// `String` (discarding tool use), returns the full text. Backs
/// [`chat_complete`] and `ipc::mentor::mentor_judge`.
pub(crate) async fn complete_once(
    app: &AppHandle,
    state: &State<'_, AppState>,
    messages: &[Value],
    system: &str,
) -> Result<String, String> {
    let (provider, betas, fallbacks) = resolve_chat(app, state).await?;
    let mut text = String::new();
    let args = sse::StreamChatArgs {
        system: Some(system),
        messages,
        tools: &[],
        betas: betas.as_deref(),
        fallbacks: fallbacks.as_deref(),
    };
    sse::stream_chat(http_client(), &provider, args, |t: &str| text.push_str(t))
        .await
        .map(|_| text)
        .map_err(|e| e.message())
}

/// `chat:send` (`index.js` ~1271-1305). See this file's module doc comment
/// for the provider-resolution/tool-loop split with `conductor::chat`.
#[tauri::command]
#[allow(unused_variables)] // brain_ws: accepted for wire-shape completeness, unused this phase — see module doc comment
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    messages: Vec<Value>,
    brain_ws: Option<String>,
    verbose: Option<bool>,
    gate: Option<bool>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:send")?;
    // Backward-compatible: the renderer lands the `verbose`/`gate` flags in
    // later slices; absent means the default (non-mentor) persona / gate on.
    let verbose = verbose.unwrap_or(false);
    let gate = gate.unwrap_or(true);

    let (provider, betas, fallbacks) = match resolve_chat(&app, &state).await {
        Ok(r) => r,
        Err(message) => {
            emit_done(&app, &id, false, Some(message));
            return Ok(json!({}));
        }
    };

    // conductor.SYSTEM + brain vault context — brain_ws-driven vault context
    // is not yet ported (see module doc comment); system is conductor's own
    // prompt alone, exactly the fallback `run_chat` would apply itself, made
    // explicit here to mirror index.js's `let system = conductor.SYSTEM`
    // assignment site. `verbose: true` swaps in the mentor (teaching)
    // persona — `conductor.mentor_system_prompt(gate)` — instead.
    let system = if verbose {
        state.conductor.mentor_system_prompt(gate)
    } else {
        state.conductor.system_prompt()
    };
    let conductor_env = conductor::env::production_env(app.clone(), provider, betas, fallbacks);

    // `run_chat` owns the whole multi-turn loop, including the abort race
    // and its OWN `chat:done` emit for every internal exit path (refusal,
    // clean end, token budget, loop limit, abort) — see that function's doc
    // comment for the exact `Ok`/`Err` split. Only a genuine, non-abort
    // stream failure reaches here as `Err`, for the same 401/authy
    // classification the JS original's outer `catch` applies.
    if let Err(err) = conductor::chat::run_chat(
        &state.conductor,
        &conductor_env,
        id.clone(),
        Some(system),
        messages,
    )
    .await
    {
        let msg = err.message();
        let authy = err.status() == Some(401) || is_authy_message(&msg);
        let friendly = if authy {
            "Chat credentials rejected. Check the provider key (MOONSHOT_API_KEY / ZHIPU_API_KEY / ANTHROPIC_API_KEY / DEEPSEEK_API_KEY / REQUESTY_API_KEY) in your shell and restart Tome.".to_string()
        } else {
            msg
        };
        emit_done(&app, &id, false, Some(friendly));
    }
    Ok(json!({}))
}

/// `chat:complete` — non-streaming one-shot completion. Used by the
/// renderer's LLM-judged comprehension gate (and anything else that needs a
/// single full-text reply without a streaming chat id). See [`complete_once`].
#[tauri::command]
pub async fn chat_complete(
    app: AppHandle,
    state: State<'_, AppState>,
    messages: Vec<Value>,
    system: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:complete")?;
    let text = complete_once(&app, &state, &messages, &system).await?;
    Ok(json!({ "text": text }))
}

/// `chat:abort` (`ipcMain.on('chat:abort', (e, id) => conductor.abortChat(id))`).
/// A no-op for an unknown/already-finished chat id — same optional-chaining
/// tolerance as `conductor.abortChat`'s `inflight.get(id)?.abort()`.
#[tauri::command]
pub async fn chat_abort(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:abort")?;
    conductor::chat::abort_chat(&state.conductor, &id);
    Ok(json!({}))
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
