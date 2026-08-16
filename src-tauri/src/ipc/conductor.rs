//! Conductor consent toggles: allow-run and per-pane allow-read. Ports
//! `src/main/conductor.js`'s consent state machine — `conductor:allowRun`/
//! `conductor:allowRead` handlers in `src/main/index.js`
//! (`ipcMain.on('conductor:allowRun', (e, v) => conductor.setAllowRun(v))`
//! / `ipcMain.on('conductor:allowRead', (e, { paneId, allowed }) =>
//! conductor.setReadConsent(paneId, !!allowed))`). Both are pure state
//! mutations on `state.conductor` — see `crate::conductor::state`'s doc
//! comment for the full consent-gate state machine (TOME-009) these two
//! setters drive.
//!
//! The THREE events `conductor.js` also owns — `conductor:readRequest`
//! (asks the user about an unconsented pane), `conductor:open` (the
//! assistant opening a pane/file), `conductor:acted` (a `type_in_terminal`
//! that actually typed something) — have no commands of their own here:
//! they are pushed from INSIDE tool dispatch during a `chat:send` call
//! (`crate::conductor::tools`' `ConductorEnv.send`), not fired independently.
//! `chat_send`'s own tool-loop integration (the task brief's other bullet
//! for this file) lives in `ipc::chat::chat_send`'s body instead, so the
//! `chat_send`/`chat_abort` commands stay registered at their existing
//! `ipc::chat` wire paths (`lock_gate::CHANNEL_OF_COMMAND` already pins
//! `chat_send -> "chat:send"`) — see that file's own doc comment for the
//! delegation into `conductor::chat::run_chat`.

use serde_json::{json, Value};
use tauri::State;

use crate::{lock_gate, state::AppState};

/// `conductor:allowRun` — `{ allow }` on the wire (`tome-ipc.js`'s
/// `allowRun: (v) => fire('conductor_allow_run', { allow: v })`).
#[tauri::command]
pub async fn conductor_allow_run(state: State<'_, AppState>, allow: bool) -> Result<Value, String> {
    lock_gate::guard(&state, "conductor:allowRun")?;
    state.conductor.set_allow_run(allow);
    Ok(json!({}))
}

/// `conductor:allowRead` — `{ paneId, allowed }` on the wire (TOME-009).
/// Names ONE pane and a boolean; there is no channel that grants/revokes
/// read access to a pane it didn't name, and this carries no scrollback
/// content, only the grant itself.
#[tauri::command]
pub async fn conductor_allow_read(
    state: State<'_, AppState>,
    pane_id: String,
    allowed: bool,
) -> Result<Value, String> {
    lock_gate::guard(&state, "conductor:allowRead")?;
    state.conductor.set_read_consent(&pane_id, allowed);
    Ok(json!({}))
}
