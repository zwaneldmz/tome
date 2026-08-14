//! Language server commands: document sync (open/change/close, fire-and-
//! forget) plus hover/definition requests. Ports `src/main/lsp.js`'s
//! `Server` class and pool as-is: `tokio::process`, hand-rolled
//! Content-Length framing, untyped `serde_json::Value` (skip
//! `lsp-types`), same 7 servers, one `lsp:missing` push per absent
//! binary. Domain logic lives in `crate::lsp` (`lsp/mod.rs`,
//! `lsp/policy.rs`) — this file's job is just `lock_gate` first (every
//! command, matching the JS original's per-handler gate), resolving
//! `state.open_folders` (the renderer's `openFolders`, threaded into
//! every JS call site as its trailing argument), and wire-shape
//! translation.

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::{lock_gate, lsp, state::AppState};

/// `lsp:didOpen` (`{ path, text }`). `ipcMain.on`, not `handle`, in the JS
/// original — fire-and-forget from the renderer's perspective
/// (`tome-ipc.js`'s `fire()`, same convention `pty_write`/`pty_resize`
/// already use over this same Tauri `invoke` transport); this command
/// still runs to completion and returns `Ok` either way.
#[tauri::command]
pub async fn lsp_did_open(app: AppHandle, state: State<'_, AppState>, path: String, text: String) -> Result<Value, String> {
    lock_gate::guard(&state, "lsp:didOpen")?;
    let folders = state.open_folders.read().expect("AppState.open_folders lock poisoned").clone();
    lsp::did_open(&app, &path, &text, &folders).await;
    Ok(json!({}))
}

/// `lsp:didChange` (`{ path, text }`).
#[tauri::command]
pub async fn lsp_did_change(app: AppHandle, state: State<'_, AppState>, path: String, text: String) -> Result<Value, String> {
    lock_gate::guard(&state, "lsp:didChange")?;
    let folders = state.open_folders.read().expect("AppState.open_folders lock poisoned").clone();
    lsp::did_change(&app, &path, &text, &folders).await;
    Ok(json!({}))
}

/// `lsp:didClose`. Wire note: the renderer's `ipcRenderer.send('lsp:
/// didClose', path)` sends a BARE path string, not `{ path }`, unlike its
/// two siblings above — but `tome-ipc.js`'s own `didClose: (path) =>
/// fire('lsp_did_close', { path })` already normalizes that into an
/// object before it crosses the Tauri `invoke` boundary, so this
/// command's signature is a plain named `path` argument like the others.
#[tauri::command]
pub async fn lsp_did_close(app: AppHandle, state: State<'_, AppState>, path: String) -> Result<Value, String> {
    lock_gate::guard(&state, "lsp:didClose")?;
    let folders = state.open_folders.read().expect("AppState.open_folders lock poisoned").clone();
    lsp::did_close(&app, &path, &folders).await;
    Ok(json!({}))
}

/// `lsp:hover` (`{ path, line, character }`) -> hover text or `null`.
#[tauri::command]
pub async fn lsp_hover(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    line: u64,
    character: u64,
) -> Result<Value, String> {
    lock_gate::guard(&state, "lsp:hover")?;
    let folders = state.open_folders.read().expect("AppState.open_folders lock poisoned").clone();
    Ok(match lsp::hover(&app, &path, line, character, &folders).await {
        Some(text) => Value::String(text),
        None => Value::Null,
    })
}

/// `lsp:definition` (`{ path, line, character }`) -> `{ path, line,
/// character }` or `null`.
#[tauri::command]
pub async fn lsp_definition(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    line: u64,
    character: u64,
) -> Result<Value, String> {
    lock_gate::guard(&state, "lsp:definition")?;
    let folders = state.open_folders.read().expect("AppState.open_folders lock poisoned").clone();
    Ok(match lsp::definition(&app, &path, line, character, &folders).await {
        Some(loc) => json!({ "path": loc.path.to_string_lossy(), "line": loc.line, "character": loc.character }),
        None => Value::Null,
    })
}
