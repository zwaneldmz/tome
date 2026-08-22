//! opencode commands: status / key-set / models / set-model. Thin
//! wire-shape translation over `crate::opencode` — `lock_gate::guard`
//! first (every command), then the domain module. Key writes are
//! write-only (the vault contract `ipc::chat` documents applies verbatim:
//! no command reads a key back). All file work is spawn_blocking'd; the
//! two process probes (`opencode --version`, `opencode models`) are async
//! in the domain module with their own timeouts.

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::{lock_gate, opencode, state::AppState};

/// `opencode:status` -> [`opencode::Status`]. Never errors: unavailability
/// is data (the Settings section renders an install hint), not a rejection.
#[tauri::command]
pub async fn opencode_status(_app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "opencode:status")?;
    let (installed, version, reason) = opencode::probe().await;
    let auth_path = opencode::auth_file();
    let config_path = opencode::config_file();
    let (auth, providers, providers_with_key, default_model) =
        tokio::task::spawn_blocking(move || {
            let auth = opencode::read_auth(&auth_path);
            let (providers, providers_with_key) = opencode::read_config_providers(&config_path);
            let default_model = opencode::read_default_model(&config_path);
            (auth, providers, providers_with_key, default_model)
        })
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(opencode::Status {
        installed,
        version,
        reason,
        auth,
        providers,
        providers_with_key,
        default_model,
    })
    .map_err(|e| e.to_string())
}

/// `opencode:key-set` (`{ provider, key }`) — writes an API credential
/// into opencode's own auth.json. Empty `key` removes the slot. Keys are
/// write-only: nothing in this file (or anywhere else in Tome) ever reads
/// one back out.
#[tauri::command]
pub async fn opencode_key_set(
    _app: AppHandle,
    state: State<'_, AppState>,
    provider: String,
    key: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "opencode:key-set")?;
    let provider = provider.trim().to_string();
    if provider.is_empty() {
        return Err("A provider id is required.".to_string());
    }
    let path = opencode::auth_file();
    tokio::task::spawn_blocking(move || opencode::set_key(&path, &provider, &key))
        .await
        .map_err(|e| e.to_string())??;
    Ok(json!({}))
}

/// `opencode:models` (`{ provider? }`) -> deduped `provider/model` ids
/// from `opencode models`. `Err` when the CLI is missing/times out — the
/// Settings section surfaces that as the list's empty state.
#[tauri::command]
pub async fn opencode_models(
    state: State<'_, AppState>,
    provider: Option<String>,
) -> Result<Vec<String>, String> {
    lock_gate::guard(&state, "opencode:models")?;
    opencode::models(provider.as_deref()).await
}

/// `opencode:set-model` (`{ model }`) — sets opencode.json's top-level
/// `model` (the CLI's default when nothing pins one). Empty clears it.
#[tauri::command]
pub async fn opencode_set_model(
    state: State<'_, AppState>,
    model: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "opencode:set-model")?;
    let path = opencode::config_file();
    tokio::task::spawn_blocking(move || opencode::set_default_model(&path, &model))
        .await
        .map_err(|e| e.to_string())??;
    Ok(json!({}))
}
