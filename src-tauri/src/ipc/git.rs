//! Git commands. Thin wrappers over `crate::git` — see that module's doc
//! comment for the Electron source these port.

use serde_json::Value;
use tauri::{AppHandle, State};

use crate::{events, lock_gate, state::AppState};

#[tauri::command]
pub async fn git_info(state: State<'_, AppState>, dir: String) -> Result<Value, String> {
    lock_gate::guard(&state, "git:info")?;
    Ok(crate::git::info(&dir).await)
}

#[tauri::command]
pub async fn git_branches(state: State<'_, AppState>, dir: String) -> Result<Value, String> {
    lock_gate::guard(&state, "git:branches")?;
    crate::git::branches(&dir).await
}

#[tauri::command]
pub async fn git_checkout(
    state: State<'_, AppState>,
    dir: String,
    branch: String,
    create: bool,
) -> Result<Value, String> {
    lock_gate::guard(&state, "git:checkout")?;
    Ok(crate::git::checkout(&dir, &branch, create).await)
}

#[tauri::command]
pub async fn git_log(
    state: State<'_, AppState>,
    dir: String,
    limit: Option<u32>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "git:log")?;
    crate::git::log(&dir, limit).await
}

#[tauri::command]
pub async fn git_commit(state: State<'_, AppState>, dir: String, hash: String) -> Result<Value, String> {
    lock_gate::guard(&state, "git:commit")?;
    crate::git::commit(&dir, &hash).await
}

#[tauri::command]
pub async fn git_diff(
    state: State<'_, AppState>,
    dir: String,
    hash: String,
    file: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "git:diff")?;
    crate::git::diff(&dir, &hash, &file).await
}

#[tauri::command]
pub async fn git_status(state: State<'_, AppState>, dir: String) -> Result<Value, String> {
    lock_gate::guard(&state, "git:status")?;
    crate::git::status(&dir).await
}

#[tauri::command]
pub async fn git_stage(
    state: State<'_, AppState>,
    dir: String,
    paths: Option<Vec<String>>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "git:stage")?;
    crate::git::stage(&dir, paths).await
}

#[tauri::command]
pub async fn git_commit_create(
    state: State<'_, AppState>,
    dir: String,
    message: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "git:commitCreate")?;
    crate::git::commit_create(&dir, &message).await
}

#[tauri::command]
pub async fn git_push(app: AppHandle, state: State<'_, AppState>, dir: String) -> Result<Value, String> {
    lock_gate::guard(&state, "git:push")?;
    events::log_event(
        &app,
        "git:push",
        vec![("dir".to_string(), serde_json::json!(dir))],
    );
    crate::git::push(&dir).await
}
