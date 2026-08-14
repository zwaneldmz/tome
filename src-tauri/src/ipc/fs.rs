//! Filesystem commands. Thin wrappers over `crate::fs` — see that module's
//! doc comment for the Electron source these port, and for why they do
//! *not* route through `crate::confine` (the original doesn't either).

use serde_json::Value;
use tauri::{AppHandle, State};

use crate::{lock_gate, state::AppState};

#[tauri::command]
pub async fn fs_read_dir(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:readDir")?;
    crate::fs::read_dir(&path).await
}

#[tauri::command]
pub async fn fs_read_file(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:readFile")?;
    crate::fs::read_file(&path).await
}

#[tauri::command]
pub async fn fs_write_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:writeFile")?;
    crate::fs::write_file(&path, &content).await
}

#[tauri::command]
pub async fn fs_mkdir(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:mkdir")?;
    crate::fs::mkdir(&path).await
}

#[tauri::command]
pub async fn fs_create_file(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:createFile")?;
    crate::fs::create_file(&path).await
}

#[tauri::command]
pub async fn fs_watch(
    state: State<'_, AppState>,
    app: AppHandle,
    path: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:watch")?;
    Ok(Value::Bool(crate::fs::watch(app, path)))
}

#[tauri::command]
pub async fn fs_unwatch(state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "fs:unwatch")?;
    crate::fs::unwatch(&path);
    Ok(Value::Null)
}
