//! Chat send/abort/providers/key management — the registry rework's
//! production caller (plan Part 4). Ports `src/main/index.js`'s
//! `chat:send`/`chat:providers`/`chat:abort` handler BODIES (not
//! `conductor.runChat`, which the handlers call into for the real
//! multi-turn work — see the scope note below), rebuilt on
//! `chat::registry`'s data-driven table, `chat::overlay`'s file shell,
//! and `chat::vault`'s one-blob key store.
//!
//! ## Provider resolution (replaces `providers.rs`'s `resolve_chat_provider`)
//!
//! [`resolve_chat`] is the single resolution path for `chat_send`,
//! `complete_once` (and through it `chat_complete`, `review.rs`,
//! `mentor.rs`): merged rows from [`chat::overlay::load_rows`], the
//! stored `chat-provider` pick, and the key ladder in [`Keys`] — vault
//! (keychain, else 0600 file) → `row.key_env[..]` in `login_env()`'s
//! secrets → the same names in process env, empty string falsy at every
//! rung. Environment may fill a selected row's key; it may never select a
//! row — the old resolver's env-override/Requesty branches, which did
//! exactly that (and reported a different provider to the UI than the one
//! actually used), are deleted. The 3-tuple return shape is preserved
//! verbatim so `review.rs`/`mentor.rs` need no edits.
//!
//! ## Keys are write-only (Cursor's contract, plan delta 3)
//!
//! `chat_key_set` stores pasted keys in the vault; there is deliberately
//! no read-back command. `chat_providers` reports only `KeyOrigin`
//! (kind + name, never the key) and a `keySet`-style boolean.
//!
//! ## Transport + tool loop (unchanged from the pre-rework file)
//!
//! OpenAI wire ports `streamOpenAI` SSE directly; Anthropic goes through a
//! hand-rolled `/v1/messages` SSE client on `reqwest` rather than the SDK.
//! Deltas stream via the event bus (see the transport note below); abort
//! delegates to `conductor::chat::abort_chat`, which cancels the
//! `CancellationToken` `conductor::Conductor` holds per chat id.
//!
//! ## Transport: event bus, not a Channel — a deliberate, noted deviation
//!
//! Every OTHER high-rate stream this rewrite ports (`pty:data`) uses a
//! Tauri `Channel` instead of `app.emit`, and that was this task's
//! starting assumption for `chat:delta` too. But `tome-ipc.js` — the
//! ALREADY-COMMITTED renderer contract this phase must not break — wires
//! `chat.onDelta`/`onDone`/`onTool` as plain `listen('chat:*', cb)` event
//! subscriptions. Switching to a `Channel` here would require a
//! `tome-ipc.js` edit to hand `chat_send` a `Channel` the way
//! `pty.create` does, so this keeps `app.emit("chat:delta", ...)`,
//! matching what the renderer already expects. Chat deltas are far
//! lower-rate than pty output, so the event bus has never been the
//! bottleneck; revisit only if volume ever proves otherwise.
//!
//! ## Tool loop: delegated to `conductor::chat::run_chat` (phase 5b)
//!
//! `chat_send` owns provider resolution / key-missing handling /
//! betas-fallbacks, then builds a `conductor::env::ConductorEnv` and calls
//! `conductor::chat::run_chat` for the actual multi-turn work — `run_chat`
//! owns the abort registry, the per-turn `tokio::select!` race, `TOOLS`,
//! tool dispatch, and the terminal `chat:done` for every internal exit
//! path; this file's own `Err` arm only ever classifies a genuine,
//! non-abort stream failure (401/authy).
//!
//! Remaining, deliberate gap (unchanged): `brain_ws`-driven vault context
//! (`brain.contextFor`) has no Rust port yet, so `system` here is
//! `conductor`'s own prompt alone, never `brain_ws`-extended.
//!
//! The renderer resends its full transcript on every `chat.send` call
//! (`panels/chat.js`'s `tome.chat.send(this.chatId, this.history,
//! brainWs)`), so server-side history is never needed either way.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::chat::registry::{self, KeyOrigin, KeySource, ProviderRow};
use crate::chat::vault::{self, Kind};
use crate::chat::{overlay, sse};
use crate::conductor;
use crate::egress::allowlist::{compile_allowlist, DEFAULT_ALLOW};
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
///
/// Built with `redirect(Policy::none())` + timeouts, NOT
/// `reqwest::Client::new()`: reqwest's `remove_sensitive_headers` strips
/// only AUTHORIZATION / COOKIE / cookie2 / PROXY_AUTHORIZATION /
/// WWW_AUTHENTICATE on a cross-host redirect — `x-api-key` is NOT on that
/// list, so a 302 from a rotated or squatted host would forward an
/// Anthropic key verbatim. Same builder `export.rs` already uses, for the
/// same reason. The 600s timeout is generous: the stream is long-lived.
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

pub(crate) fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client builds from static, always-valid config")
    })
}

/// The key ladder behind [`registry::KeySource`] (plan §4.2): vault
/// (keychain, else 0600 file) → `row.key_env[..]` in `login_env()`
/// secrets → the same names in process env. Empty string is falsy at
/// every rung (`truthy`, preserved). `KeyOrigin` is reported to the UI
/// and never carries the key itself — it turns two mysteries into facts:
/// "why is the dot hollow when I definitely exported that variable" and
/// "the UI says GLM but every request goes to router.requesty.ai".
struct Keys<'a> {
    vault_keys: &'a HashMap<String, String>,
    kind: Kind,
    secrets: &'a HashMap<String, String>,
    env: &'a HashMap<String, String>,
}

fn truthy(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

impl registry::KeySource for Keys<'_> {
    fn key_for(&self, row: &ProviderRow) -> Option<(String, KeyOrigin)> {
        if let Some(k) = self.vault_keys.get(&row.id).and_then(|v| truthy(v)) {
            return Some((
                k,
                match self.kind {
                    Kind::Keychain => KeyOrigin::Keychain,
                    Kind::File => KeyOrigin::File,
                },
            ));
        }
        for name in &row.key_env {
            if let Some(k) = self.secrets.get(name).and_then(|v| truthy(v)) {
                return Some((k, KeyOrigin::Shell(name.clone())));
            }
        }
        for name in &row.key_env {
            if let Some(k) = self.env.get(name).and_then(|v| truthy(v)) {
                return Some((k, KeyOrigin::Env(name.clone())));
            }
        }
        None
    }
}

/// Whether an agent pane's egress sandbox could reach a row's host.
/// Informational only — Tome's own chat calls are NOT proxied through the
/// pane gateways (user-added providers work for chat the moment they're
/// saved); this exists so the card can honestly say "agent panes cannot
/// reach this host" and point at Security → Egress instead of silently
/// pretending the sandbox grew with the registry.
fn agent_egress(base_url: &str) -> &'static str {
    let Some(host) = reqwest::Url::parse(base_url).ok().and_then(|u| {
        u.host_str()
            .map(str::to_string)
            .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
    }) else {
        return "not-allowlisted";
    };
    let allow = compile_allowlist(DEFAULT_ALLOW);
    if allow.iter().any(|m| m.matches(&host)) {
        "allowed"
    } else {
        "not-allowlisted"
    }
}

/// `chat:providers` (`ipcMain.handle('chat:providers', ...)`). The full
/// provider list for Preferences: every merged row (id, label, model,
/// models, host, alternates, egress reachability, last-send error), the
/// stored pick, and the resolved `effective` row + `reason` when nothing
/// is resolved. The key ITSELF never crosses IPC: only `keyOrigin`
/// (kind + name) and a presence boolean.
#[tauri::command]
pub async fn chat_providers(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:providers")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");

    let login = login_env::login_env().await;
    let env: HashMap<String, String> = std::env::vars().collect();
    let (stored, rows) = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            (
                store::get(&dir, "chat-provider", locked),
                overlay::load_rows(&dir),
            )
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let stored_str = stored.as_str().map(str::trim).filter(|s| !s.is_empty());
    let (vault_keys, kind) = state
        .chat_keys
        .read()
        .expect("AppState.chat_keys lock poisoned")
        .clone();
    let keys = Keys {
        vault_keys: &vault_keys,
        kind,
        secrets: &login.secrets,
        env: &env,
    };
    let resolution = registry::resolve(&rows, stored_str, &keys);
    let last_errors = state
        .chat_last_error
        .lock()
        .expect("AppState.chat_last_error lock poisoned");

    let list: Vec<Value> = rows
        .iter()
        .map(|p| {
            let found = keys.key_for(p);
            json!({
                "id": p.id,
                "label": p.label,
                "model": p.model,
                "models": p.models,
                "baseUrl": p.base_url,
                "alternates": p.alternates,
                "keyOrigin": key_origin_value(found.map(|(_, o)| o)),
                "active": stored_str == Some(p.id.as_str()),
                "agentEgress": agent_egress(&p.base_url),
                "lastError": last_errors.get(&p.id).cloned(),
                "builtin": p.builtin,
            })
        })
        .collect();

    let (effective, reason) = match &resolution {
        registry::Resolution::Ready(p) => (
            Some(json!({
                "id": p.id,
                "label": p.label,
                "model": p.model,
                "host": p.base_url,
                "keyOrigin": key_origin_value(Some(p.key_origin.clone())),
            })),
            Value::Null,
        ),
        registry::Resolution::NoneChosen => (
            None,
            json!("No provider chosen — pick one and paste a key."),
        ),
        registry::Resolution::Unknown { id } => (
            None,
            json!(format!(
                "The provider you saved ({id}) is no longer in your list."
            )),
        ),
        registry::Resolution::NoKey {
            label, key_env, ..
        } => {
            let env_hint = if key_env.is_empty() {
                String::new()
            } else {
                format!(
                    " (or set {} in your shell and restart)",
                    key_env.join(" / ")
                )
            };
            (
                None,
                json!(format!(
                    "{label} needs a key — paste one in \u{2318}, \u{2192} Assistant{env_hint}."
                )),
            )
        }
    };

    Ok(json!({
        "providers": list,
        "active": stored_str,
        "effective": effective,
        "reason": reason,
    }))
}

/// `KeyOrigin`'s UI shape: `{ kind, name }`, `name` null for the nameless
/// kinds — never carries the key.
fn key_origin_value(origin: Option<KeyOrigin>) -> Value {
    match origin {
        None => Value::Null,
        Some(o) => match serde_json::to_value(&o) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
    }
}

/// `chat:key-set` — write-only key storage (plan §4.3, delta 3). Trims
/// the pasted key; an empty/whitespace save REMOVES the slot (empty
/// string is falsy at every ladder rung, so storing one would only
/// produce a 401). Saves the whole vault blob and replaces the
/// `AppState.chat_keys` snapshot. There is deliberately no read-back
/// command.
#[tauri::command]
pub async fn chat_key_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    key: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:key-set")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("A provider id is required.".to_string());
    }
    let key = key.trim().to_string();

    let (mut map, _) = state
        .chat_keys
        .read()
        .expect("AppState.chat_keys lock poisoned")
        .clone();
    if key.is_empty() {
        map.remove(&id);
    } else {
        map.insert(id.clone(), key);
    }

    let kind = {
        let dir = dir.clone();
        // Clone, not borrow: spawn_blocking requires 'static, and a
        // provider-key map is a handful of small strings at most.
        let map_for_save = map.clone();
        tokio::task::spawn_blocking(move || {
            let vault = vault::Vault::new(&dir);
            vault.save(&map_for_save)
        })
        .await
        .map_err(|e| e.to_string())??
    };

    *state
        .chat_keys
        .write()
        .expect("AppState.chat_keys lock poisoned") = (map, kind);
    Ok(json!({}))
}

/// `chat:provider-add` — the "+ Add provider" form's write path (the
/// fourth command; the handover's three-command list missed that the
/// add-provider UI needs one). Builds an `added` row from the form's
/// fields, vets the base URL (https, or http for loopback only, no
/// embedded credentials, no metadata hosts), mints a slug id from the
/// label (deduped with a numeric suffix), and returns it so the renderer
/// can follow up with `chat:key-set` for the pasted key.
#[tauri::command]
pub async fn chat_provider_add(
    app: AppHandle,
    state: State<'_, AppState>,
    label: String,
    base_url: String,
    model: String,
    wire: String,
    auth: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:provider-add")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    let label = label.trim().to_string();
    let model = model.trim().to_string();
    if label.is_empty() || model.is_empty() {
        return Err("Label and model are required.".to_string());
    }
    let wire = match wire.trim() {
        "anthropic" => registry::Wire::Anthropic,
        _ => registry::Wire::OpenAi,
    };
    let auth = match auth.as_deref().map(str::trim) {
        Some("x-api-key") => registry::Auth::XApiKey,
        _ => registry::Auth::Bearer,
    };
    let base_url = registry::vet_base_url(&base_url)?;

    let row = registry::ProviderRow {
        id: String::new(), // minted below against the existing rows
        label,
        wire,
        auth,
        base_url,
        model: model.clone(),
        models: vec![model.clone()],
        models_url: None,
        alternates: vec![],
        key_env: vec![],
        max_output_tokens: None,
        betas: vec![],
        builtin: false,
    };

    let id = tokio::task::spawn_blocking(move || {
        let rows = overlay::load_rows(&dir);
        let mut ov = overlay::load_overlay(&dir);
        let id = mint_added_id(&rows, &ov, &row.label);
        let mut row = row;
        row.id = id.clone();
        ov.added.push(row);
        overlay::save_overlay(&dir, &ov)?;
        Ok::<String, String>(id)
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(json!({ "id": id }))
}

/// Mints a collision-free id for a user-added row: a slug of the label
/// (`[a-z0-9-]`, collapsed dashes, never empty — "My Local vLLM" →
/// "my-local-vllm"), with a `-2`/`-3`… suffix when the slug is taken by
/// a built-in or an existing added row. Pure — unit-testable.
pub(crate) fn mint_added_id(rows: &[ProviderRow], ov: &registry::Overlay, label: &str) -> String {
    let taken: Vec<&str> = rows
        .iter()
        .map(|r| r.id.as_str())
        .chain(ov.added.iter().map(|r| r.id.as_str()))
        .collect();
    let mut slug: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "provider".to_string();
    }
    if !taken.iter().any(|t| *t == slug) {
        return slug;
    }
    for n in 2.. {
        let candidate = format!("{slug}-{n}");
        if !taken.iter().any(|t| *t == candidate) {
            return candidate;
        }
    }
    unreachable!("the suffix loop always terminates")
}

/// The overlay patch `chat:provider-set` accepts — `{ model?, region?,
/// hidden? }`, all optional, never a sparse patch over arbitrary row
/// fields (a built-in's `base_url` is reachable only through `region`,
/// and only to a value that row's compiled-in `alternates` contains).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPatch {
    #[serde(default)]
    model: Option<Option<String>>,
    #[serde(default)]
    region: Option<Option<String>>,
    #[serde(default)]
    hidden: Option<bool>,
}

impl ProviderPatch {
    fn is_empty(&self) -> bool {
        self.model.is_none() && self.region.is_none() && self.hidden.is_none()
    }
}

/// Applies a validated patch to the overlay in memory. Pure — the caller
/// persists. `rows` is the merged table (for region validation and the
/// builtin/added distinction).
fn apply_patch(
    rows: &[ProviderRow],
    ov: &mut registry::Overlay,
    id: &str,
    patch: &ProviderPatch,
) -> Result<(), String> {
    let Some(row) = rows.iter().find(|r| r.id == id) else {
        return Err(format!("Unknown provider id: {id}"));
    };

    if let Some(model) = &patch.model {
        match model {
            Some(m) => {
                let trimmed = m.trim();
                if trimmed.is_empty() {
                    return Err("Model must be a non-empty string.".to_string());
                }
                ov.model.insert(id.to_string(), trimmed.to_string());
            }
            None => {
                ov.model.remove(id);
            }
        }
    }

    if let Some(region) = &patch.region {
        match region {
            Some(r) => {
                if !row.alternates.iter().any(|a| &a.base_url == r) {
                    return Err(
                        "That region is not available for this provider.".to_string()
                    );
                }
                ov.region.insert(id.to_string(), r.clone());
            }
            None => {
                ov.region.remove(id);
            }
        }
    }

    if let Some(hidden) = patch.hidden {
        if !row.builtin {
            return Err("User-added providers can be deleted, not hidden.".to_string());
        }
        if hidden {
            if !ov.hidden.iter().any(|h| h == id) {
                ov.hidden.push(id.to_string());
            }
        } else {
            ov.hidden.retain(|h| h != id);
        }
    }

    Ok(())
}

/// `chat:provider-set` — writes the user overlay (`chat-providers.json`,
/// reserved): per-row model override, region pin (validated against the
/// row's compiled-in alternates), and hide/show for built-ins.
#[tauri::command]
pub async fn chat_provider_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    patch: ProviderPatch,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:provider-set")?;
    if patch.is_empty() {
        return Err("Nothing to set — pass model, region, or hidden.".to_string());
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let id = id.trim().to_string();

    tokio::task::spawn_blocking(move || {
        let rows = overlay::load_rows(&dir);
        let mut ov = overlay::load_overlay(&dir);
        apply_patch(&rows, &mut ov, &id, &patch)?;
        overlay::save_overlay(&dir, &ov)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(json!({}))
}

/// `chat:provider-delete` — removes a user-added row AND its vault slot.
/// Built-ins are protected: hiding is the only way to remove one from the
/// list (their ids are reserved — see `registry::merge`'s doc comment on
/// why an overlay must never be able to resurrect a built-in id).
#[tauri::command]
pub async fn chat_provider_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:provider-delete")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let id = id.trim().to_string();

    tokio::task::spawn_blocking(move || {
        let mut ov = overlay::load_overlay(&dir);
        if overlay::load_defaults().iter().any(|r| r.id == id) {
            return Err("Built-in providers can be hidden, not deleted.".to_string());
        }
        let before = ov.added.len();
        ov.added.retain(|r| r.id != id);
        if ov.added.len() == before {
            return Err(format!("Unknown provider id: {id}"));
        }
        ov.model.remove(&id);
        ov.region.remove(&id);
        ov.hidden.retain(|h| h != &id);
        overlay::save_overlay(&dir, &ov)?;
        let vault = vault::Vault::new(&dir);
        let mut map = vault.load().0;
        map.remove(&id);
        vault.save(&map)?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;

    // Refresh the in-memory snapshot to match the vault the deletion just
    // wrote (only reachable when the keychain path was taken — the file
    // fallback is reloaded lazily, but consistency is cheaper than
    // reasoning about which path won).
    {
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let (map, kind) = tokio::task::spawn_blocking(move || vault::Vault::new(&dir).load())
            .await
            .map_err(|e| e.to_string())?;
        *state
            .chat_keys
            .write()
            .expect("AppState.chat_keys lock poisoned") = (map, kind);
    }
    Ok(json!({}))
}

/// Resolves the active provider + beta/fallback flags against the merged
/// registry. Mirrors `chat_send`'s inline resolution, factored out so
/// `chat_send`, `complete_once`, and `mentor_judge` share one resolution
/// path. Returns a friendly error string for the three non-Ready states
/// (NoneChosen / Unknown / NoKey). The 3-tuple return shape is preserved
/// verbatim for `review.rs`/`mentor.rs`; `betas`/`fallbacks` are built
/// from the row's `betas` (empty in every shipped row — the old
/// wire-predicate `beta: matches!(wire, Anthropic)` attached beta-only
/// body params to the GA endpoint and 400'd).
pub(crate) async fn resolve_chat(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<
    (
        registry::ResolvedProvider,
        Option<Vec<String>>,
        Option<String>,
    ),
    String,
> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let login = login_env::login_env().await;
    let env: HashMap<String, String> = std::env::vars().collect();
    let (stored, rows) = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            (
                store::get(&dir, "chat-provider", locked),
                overlay::load_rows(&dir),
            )
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let stored_str = stored.as_str().map(str::trim).filter(|s| !s.is_empty());
    let (vault_keys, kind) = state
        .chat_keys
        .read()
        .expect("AppState.chat_keys lock poisoned")
        .clone();
    let keys = Keys {
        vault_keys: &vault_keys,
        kind,
        secrets: &login.secrets,
        env: &env,
    };

    let provider = match registry::resolve(&rows, stored_str, &keys) {
        registry::Resolution::NoneChosen => {
            return Err(
                "No provider chosen — pick one in \u{2318}, \u{2192} Assistant and paste a key."
                    .to_string(),
            );
        }
        registry::Resolution::Unknown { id } => {
            return Err(format!(
                "The provider you saved ({id}) is no longer in your list — pick another in \u{2318}, \u{2192} Assistant."
            ));
        }
        registry::Resolution::NoKey {
            label, key_env, ..
        } => {
            let env_hint = if key_env.is_empty() {
                String::new()
            } else {
                format!(
                    " (or set {} in your shell and restart)",
                    key_env.join(" / ")
                )
            };
            return Err(format!(
                "{label} needs a key — paste one in \u{2318}, \u{2192} Assistant{env_hint}."
            ));
        }
        registry::Resolution::Ready(p) => p,
    };

    let (betas, fallbacks): (Option<Vec<String>>, Option<String>) = if provider.betas.is_empty() {
        (None, None)
    } else {
        (
            Some(provider.betas.clone()),
            Some("default".to_string()),
        )
    };

    Ok((provider, betas, fallbacks))
}

/// Non-streaming one-shot completion: resolves the provider, streams into
/// a `String` (discarding tool use), returns the full text. Backs
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
#[allow(clippy::too_many_arguments)] // kept flat for consistency with the existing verbose/gate args; a struct arg would deserialize fine but is churn mid-contract
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    messages: Vec<Value>,
    brain_ws: Option<String>,
    verbose: Option<bool>,
    gate: Option<bool>,
    voice: Option<bool>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:send")?;
    // Backward-compatible: the renderer lands the `verbose`/`gate`/`voice`
    // flags in later slices; absent means the default (non-mentor) persona /
    // gate on / non-voice session.
    let verbose = verbose.unwrap_or(false);
    let gate = gate.unwrap_or(true);
    let voice = voice.unwrap_or(false);

    let (provider, betas, fallbacks) = match resolve_chat(&app, &state).await {
        Ok(r) => r,
        Err(message) => {
            emit_done(&app, &id, false, Some(message));
            return Ok(json!({}));
        }
    };

    // A send against this row starts: its previous rejection note (if any)
    // is about to be re-tested — clear it now, set it again only if the
    // row fails again (delta 2).
    state
        .chat_last_error
        .lock()
        .expect("AppState.chat_last_error lock poisoned")
        .remove(&provider.id);

    // conductor.SYSTEM + brain vault context — brain_ws-driven vault
    // context is not yet ported (see module doc comment); system is
    // conductor's own prompt alone. `voice: true` swaps in the voice
    // session persona; `verbose: true` the mentor persona.
    let system = if voice {
        state.conductor.voice_system_prompt()
    } else if verbose {
        state.conductor.mentor_system_prompt(gate)
    } else {
        state.conductor.system_prompt()
    };
    let conductor_env = conductor::env::production_env(app.clone(), provider.clone(), betas, fallbacks);

    // `run_chat` owns the whole multi-turn loop, including the abort race
    // and its OWN `chat:done` emit for every internal exit path (refusal,
    // clean end, token budget, loop limit, abort). Only a genuine,
    // non-abort stream failure reaches here as `Err`, for the same
    // 401/authy classification the JS original's outer `catch` applies.
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
        if authy {
            // Fail loudly at the row that failed (Cursor's
            // fail-at-request-time model): the card shows this until the
            // next send against the row starts.
            let friendly = format!(
                "Chat credentials rejected — check the {} key (\u{2318}, \u{2192} Assistant) and try again.",
                provider.label
            );
            state
                .chat_last_error
                .lock()
                .expect("AppState.chat_last_error lock poisoned")
                .insert(provider.id.clone(), friendly.clone());
            emit_done(&app, &id, false, Some(friendly));
        } else {
            emit_done(&app, &id, false, Some(msg));
        }
    }
    Ok(json!({}))
}

/// `chat:complete` — non-streaming one-shot completion. Used by the
/// renderer's LLM-judged comprehension gate (and anything else that needs
/// a single full-text reply without a streaming chat id). See [`complete_once`].
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

/// `chat:history-list` (`{ query? }`) — the searchable conversation
/// archive. A workspace startup starts the assistant FRESH (a new chat id
/// per restored pane); this is how the old conversations stay reachable.
/// Scans the store dir for `chat-log-*.json` (name-vetted by
/// [`crate::chat::history::chat_id_of_file_name`], so nothing but a valid
/// log key is ever read), filters case-insensitively over the raw payload,
/// summarizes (id · count · first-user snippet · mtime), and returns
/// newest-first. Reading happens on the blocking pool — a store dir with
/// many logs is many small reads, not a hot path.
#[tauri::command]
pub async fn chat_history_list(
    app: AppHandle,
    state: State<'_, AppState>,
    query: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "chat:history-list")?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let query = query.unwrap_or_default();
    tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut entries = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            return Ok(json!([]));
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = crate::chat::history::chat_id_of_file_name(&name) else {
                continue;
            };
            let mtime_ms = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let Ok(payload) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if !crate::chat::history::log_matches(&payload, &query) {
                continue;
            }
            if let Some(v) = crate::chat::history::summarize_log(&id, &payload, mtime_ms) {
                entries.push(v);
            }
        }
        crate::chat::history::sort_newest_first(&mut entries);
        Ok(Value::Array(entries))
    })
    .await
    .map_err(|e| e.to_string())?
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
    use crate::chat::registry::{Auth, Wire};

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

    // ================= the key ladder (Keys / registry::KeySource) =================

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn glm_row() -> ProviderRow {
        ProviderRow {
            id: "glm".to_string(),
            label: "GLM (Z.ai)".to_string(),
            wire: Wire::OpenAi,
            auth: Auth::Bearer,
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            model: "glm-5.3".to_string(),
            models: vec![],
            models_url: None,
            alternates: vec![],
            key_env: vec!["ZAI_API_KEY".to_string(), "ZHIPU_API_KEY".to_string()],
            max_output_tokens: None,
            betas: vec![],
            builtin: true,
        }
    }

    #[test]
    fn keys_prefer_the_vault_over_shell_and_env() {
        let row = glm_row();
        let keys = Keys {
            vault_keys: &env(&[("glm", "vault-key")]),
            kind: Kind::Keychain,
            secrets: &env(&[("ZAI_API_KEY", "shell-key")]),
            env: &env(&[("ZHIPU_API_KEY", "env-key")]),
        };
        assert_eq!(
            keys.key_for(&row),
            Some(("vault-key".to_string(), KeyOrigin::Keychain))
        );
    }

    #[test]
    fn keys_report_the_file_kind_when_the_vault_blob_came_from_disk() {
        let row = glm_row();
        let keys = Keys {
            vault_keys: &env(&[("glm", "file-key")]),
            kind: Kind::File,
            secrets: &HashMap::new(),
            env: &HashMap::new(),
        };
        assert_eq!(
            keys.key_for(&row),
            Some(("file-key".to_string(), KeyOrigin::File))
        );
    }

    #[test]
    fn keys_fall_to_secrets_then_env_in_key_env_order() {
        let row = glm_row();
        // First env name absent → second name in secrets wins.
        let keys = Keys {
            vault_keys: &HashMap::new(),
            kind: Kind::File,
            secrets: &env(&[("ZHIPU_API_KEY", "zhipu-shell")]),
            env: &HashMap::new(),
        };
        assert_eq!(
            keys.key_for(&row),
            Some(("zhipu-shell".to_string(), KeyOrigin::Shell("ZHIPU_API_KEY".to_string())))
        );

        // Secrets absent → process env.
        let keys = Keys {
            vault_keys: &HashMap::new(),
            kind: Kind::File,
            secrets: &HashMap::new(),
            env: &env(&[("ZAI_API_KEY", "zai-env")]),
        };
        assert_eq!(
            keys.key_for(&row),
            Some(("zai-env".to_string(), KeyOrigin::Env("ZAI_API_KEY".to_string())))
        );
    }

    #[test]
    fn keys_treat_an_empty_string_as_absent_at_every_rung() {
        let row = glm_row();
        let keys = Keys {
            vault_keys: &env(&[("glm", "  ")]),
            kind: Kind::File,
            secrets: &env(&[("ZAI_API_KEY", "")]),
            env: &env(&[("ZHIPU_API_KEY", "  ")]),
        };
        assert_eq!(keys.key_for(&row), None);
    }

    #[test]
    fn keys_never_fall_through_to_an_unrelated_env_name() {
        // A stray OPENAI_API_KEY must not satisfy the glm row (the old
        // resolver's ambient fallback — deleted).
        let row = glm_row();
        let keys = Keys {
            vault_keys: &HashMap::new(),
            kind: Kind::File,
            secrets: &HashMap::new(),
            env: &env(&[("OPENAI_API_KEY", "sk-x")]),
        };
        assert_eq!(keys.key_for(&row), None);
    }

    // ================= agent_egress =================

    #[test]
    fn agent_egress_allowlists_only_hosts_in_default_allow() {
        assert_eq!(agent_egress("https://api.anthropic.com"), "allowed");
        assert_eq!(agent_egress("https://openrouter.ai/api/v1"), "allowed");
        // not in DEFAULT_ALLOW (frozen at 16 — plan Q3)
        assert_eq!(agent_egress("https://api.z.ai/api/paas/v4"), "not-allowlisted");
        assert_eq!(agent_egress("https://api.groq.com"), "allowed");
        assert_eq!(agent_egress("not a url"), "not-allowlisted");
    }

    // ================= apply_patch =================

    fn rows() -> Vec<ProviderRow> {
        overlay::load_defaults()
    }

    #[test]
    fn apply_patch_rejects_a_model_of_whitespace() {
        let mut ov = registry::Overlay::default();
        let patch = ProviderPatch {
            model: Some(Some("   ".to_string())),
            ..Default::default()
        };
        assert!(apply_patch(&rows(), &mut ov, "glm", &patch).is_err());
    }

    #[test]
    fn apply_patch_applies_and_clears_model_overrides() {
        let mut ov = registry::Overlay::default();
        let set = ProviderPatch {
            model: Some(Some("glm-5-turbo".to_string())),
            ..Default::default()
        };
        apply_patch(&rows(), &mut ov, "glm", &set).unwrap();
        assert_eq!(ov.model.get("glm").unwrap(), "glm-5-turbo");

        let clear = ProviderPatch {
            model: Some(None),
            ..Default::default()
        };
        apply_patch(&rows(), &mut ov, "glm", &clear).unwrap();
        assert!(!ov.model.contains_key("glm"));
    }

    #[test]
    fn apply_patch_rejects_a_region_outside_the_rows_alternates() {
        let mut ov = registry::Overlay::default();
        let patch = ProviderPatch {
            region: Some(Some("https://evil.example.com/v1".to_string())),
            ..Default::default()
        };
        assert!(apply_patch(&rows(), &mut ov, "glm", &patch).is_err());
    }

    #[test]
    fn apply_patch_honors_a_compiled_in_region() {
        let mut ov = registry::Overlay::default();
        let patch = ProviderPatch {
            region: Some(Some("https://open.bigmodel.cn/api/paas/v4".to_string())),
            ..Default::default()
        };
        apply_patch(&rows(), &mut ov, "glm", &patch).unwrap();
        assert_eq!(
            ov.region.get("glm").unwrap(),
            "https://open.bigmodel.cn/api/paas/v4"
        );
    }

    #[test]
    fn apply_patch_hides_and_unhides_a_builtin() {
        let mut ov = registry::Overlay::default();
        let hide = ProviderPatch {
            hidden: Some(true),
            ..Default::default()
        };
        apply_patch(&rows(), &mut ov, "openai", &hide).unwrap();
        assert!(ov.hidden.contains(&"openai".to_string()));

        let show = ProviderPatch {
            hidden: Some(false),
            ..Default::default()
        };
        apply_patch(&rows(), &mut ov, "openai", &show).unwrap();
        assert!(!ov.hidden.contains(&"openai".to_string()));
    }

    #[test]
    fn apply_patch_rejects_unknown_ids_and_hiding_added_rows() {
        let mut ov = registry::Overlay::default();
        let patch = ProviderPatch {
            model: Some(Some("m".to_string())),
            ..Default::default()
        };
        assert!(apply_patch(&rows(), &mut ov, "bogus", &patch).is_err());

        let hide = ProviderPatch {
            hidden: Some(true),
            ..Default::default()
        };
        let added = ProviderRow {
            id: "myai".to_string(),
            label: "My AI".to_string(),
            wire: Wire::OpenAi,
            auth: Auth::Bearer,
            base_url: "https://myai.example.com/v1".to_string(),
            model: "m1".to_string(),
            models: vec![],
            models_url: None,
            alternates: vec![],
            key_env: vec![],
            max_output_tokens: None,
            betas: vec![],
            builtin: false,
        };
        let mut rows = rows();
        rows.push(added);
        assert!(apply_patch(&rows, &mut ov, "myai", &hide).is_err());
    }

    // ================= mint_added_id =================

    #[test]
    fn mint_added_id_slugifies_the_label() {
        let ov = registry::Overlay::default();
        assert_eq!(mint_added_id(&rows(), &ov, "My Local vLLM"), "my-local-vllm");
        assert_eq!(mint_added_id(&rows(), &ov, "Groq!"), "groq");
    }

    #[test]
    fn mint_added_id_never_collides_with_a_builtin_or_added_row() {
        let ov = registry::Overlay::default();
        assert_eq!(mint_added_id(&rows(), &ov, "OpenAI"), "openai-2");
        assert_eq!(mint_added_id(&rows(), &ov, "Kimi"), "kimi-2");

        let mut ov = registry::Overlay::default();
        ov.added.push(ProviderRow {
            id: "my-ai".to_string(),
            label: "My AI".to_string(),
            wire: Wire::OpenAi,
            auth: Auth::Bearer,
            base_url: "https://myai.example.com/v1".to_string(),
            model: "m1".to_string(),
            models: vec![],
            models_url: None,
            alternates: vec![],
            key_env: vec![],
            max_output_tokens: None,
            betas: vec![],
            builtin: false,
        });
        let mut all = rows();
        all.push(ov.added[0].clone());
        assert_eq!(mint_added_id(&all, &ov, "My AI"), "my-ai-2");
    }

    #[test]
    fn mint_added_id_falls_back_to_provider_for_an_unsluggable_label() {
        let ov = registry::Overlay::default();
        assert_eq!(mint_added_id(&rows(), &ov, "!!!"), "provider");
    }
}
