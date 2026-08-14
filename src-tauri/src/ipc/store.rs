//! JSON key-value store commands. Thin wrappers over `crate::store` (key
//! vetting via `crate::store_keys`) — see those modules' doc comments for
//! the Electron source these port.

use tauri::{AppHandle, Manager, State};

use crate::state::AppState;
use crate::{lock_gate, store};

/// Ports `index.js`'s `ipcMain.handle('store:get', (e, key) =>
/// readStore(key))`. Never fails — a disallowed key or an
/// unreadable/corrupt file both resolve to `Ok(Value::Null)`, exactly like
/// the Electron original's `readStore`.
#[tauri::command]
pub async fn store_get(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "store:get")?;
    let locked = *state.locked.read().unwrap();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let value = tokio::task::spawn_blocking(move || store::get(&dir, &key, locked))
        .await
        .map_err(|e| e.to_string())?;
    Ok(value)
}

/// Ports `index.js`'s `store:set` handler. `Err` carries the exact Electron
/// message text (`"Bad store key."` / `"Locked."`) — the renderer shim
/// (`tome-ipc.js`'s `call()`) turns a command `Err(String)` into a real
/// `Error(message)`, so `err.message` on the renderer side reads
/// identically to the thrown-`Error` original. On success there is nothing
/// meaningful to return (the Electron handler returns `undefined`; no
/// renderer call site reads the resolved value), so this returns
/// `Value::Null`.
#[tauri::command]
pub async fn store_set(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "store:set")?;
    let locked = *state.locked.read().unwrap();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || store::set(&dir, &key, &value, locked))
        .await
        .map_err(|e| e.to_string())??;
    Ok(serde_json::Value::Null)
}
