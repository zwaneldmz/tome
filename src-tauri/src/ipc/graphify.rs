//! Graphify commands: status/build/cancel/query/path/explain/affected.
//! Thin wire-shape translation over `crate::graphify` — `lock_gate::guard`
//! first (every command, same discipline as `ipc::brain`), confine the
//! workspace the renderer names against the open workspace folders (the
//! renderer is free text; a path outside `open_folders` must never reach a
//! spawn), then hand off to the domain module. Builds stream through the
//! renderer's Tauri Channel (`on_line`), the same shape `ipc::pty` uses
//! for pty data.
//!
//! All commands take the workspace as an explicit renderer argument rather
//! than resolving it from `open_folders` themselves — the pane already
//! knows its ws (it is spawned per workspace, `graphify:<dir>`), and
//! picking `open_folders[0]` here would silently target the wrong folder
//! in a multi-folder workspace.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::State;

use crate::{confine, graphify, lock_gate, state::AppState};

/// Confines `ws` against the open workspace folders, mirroring the
/// two-branch message `ipc::shell`'s `confinement_error` builds. Duplicated
/// locally — `confine.rs` returns bare `Err` strings and this file wants
/// the `graphify:` prefix on them.
fn confine_ws(state: &State<'_, AppState>, ws: &str) -> Result<PathBuf, String> {
    match confine::confined_real_path(state, Path::new(ws)) {
        Ok(p) => Ok(p),
        Err(reason) => Err(format!("graphify: {reason}")),
    }
}

/// `graphify:status` (`{ ws }`) -> [`graphify::Status`]. Never errors:
/// unavailability is data, not a rejection.
#[tauri::command]
pub async fn graphify_status(state: State<'_, AppState>, ws: String) -> Result<Value, String> {
    lock_gate::guard(&state, "graphify:status")?;
    let ws = confine_ws(&state, &ws)?;
    serde_json::to_value(graphify::status(&ws).await).map_err(|e| e.to_string())
}

/// `graphify:build` (`{ ws, onLine: Channel }`) -> one-line summary.
/// Streams each stdout/stderr line of both pipeline stages down the
/// channel as a plain string. `Err` when the workspace is unconfined, a
/// build is already running, or a stage exits non-zero (the channel has
/// already carried the stage's tail at that point).
#[tauri::command]
pub async fn graphify_build(
    state: State<'_, AppState>,
    ws: String,
    on_line: Channel<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "graphify:build")?;
    let ws = confine_ws(&state, &ws)?;
    let result = graphify::build(&ws, move |line| {
        let _ = on_line.send(line);
    })
    .await?;
    Ok(json!({ "summary": result }))
}

/// `graphify:cancel` — kills the in-flight build stage, if any. Resolves
/// `{ killed: bool }`; killing a stage makes `graphify:build` reject with
/// that stage's non-zero exit, which the pane renders as "cancelled".
#[tauri::command]
pub async fn graphify_cancel(state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "graphify:cancel")?;
    Ok(json!({ "killed": graphify::cancel().await }))
}

/// Shared impl for the four read-only queries: confine, then `ask` with
/// the right arg vector. Returns the CLI's plain text (already
/// output-capped by the domain module).
async fn ask(state: &State<'_, AppState>, ws: String, args: Vec<String>) -> Result<String, String> {
    let ws = confine_ws(state, &ws)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    graphify::ask(&ws, &refs).await
}

/// `graphify:query` (`{ ws, question }`) -> BFS traversal text.
#[tauri::command]
pub async fn graphify_query(
    state: State<'_, AppState>,
    ws: String,
    question: String,
) -> Result<String, String> {
    lock_gate::guard(&state, "graphify:query")?;
    ask(&state, ws, vec!["query".into(), question]).await
}

/// `graphify:path` (`{ ws, from, to }`) -> shortest-path text.
#[tauri::command]
pub async fn graphify_path(
    state: State<'_, AppState>,
    ws: String,
    from: String,
    to: String,
) -> Result<String, String> {
    lock_gate::guard(&state, "graphify:path")?;
    ask(&state, ws, vec!["path".into(), from, to]).await
}

/// `graphify:explain` (`{ ws, symbol }`) -> node + neighbors explanation.
#[tauri::command]
pub async fn graphify_explain(
    state: State<'_, AppState>,
    ws: String,
    symbol: String,
) -> Result<String, String> {
    lock_gate::guard(&state, "graphify:explain")?;
    ask(&state, ws, vec!["explain".into(), symbol]).await
}

/// `graphify:affected` (`{ ws, symbol }`) -> reverse-traversal impact text.
#[tauri::command]
pub async fn graphify_affected(
    state: State<'_, AppState>,
    ws: String,
    symbol: String,
) -> Result<String, String> {
    lock_gate::guard(&state, "graphify:affected")?;
    ask(&state, ws, vec!["affected".into(), symbol]).await
}
