//! Persistent event log read. Thin wrapper over `crate::events` (backed by
//! the pure core in `crate::eventlog`) — see those modules' doc comments.

use tauri::{AppHandle, State};

use crate::state::AppState;
use crate::{events, lock_gate};

/// Ports `index.js`'s `ipcMain.handle('events:list', () =>
/// events.readEvents())` — read-only tail of the persistent event log, most
/// recent `eventlog::TAIL` records, oldest-first.
#[tauri::command]
pub async fn events_list(app: AppHandle, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "events:list")?;
    Ok(serde_json::Value::Array(events::list(&app).await))
}
