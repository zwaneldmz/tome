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

/// `conductor:cwd` (`{ root }`) — the renderer's ACTIVE workspace root.
/// The conductor's workspace-relative tools (graph queries, `run_agent`,
/// flow reads/drafts) operate at the root you are LOOKING at, not the first
/// folder of the first workspace. `root: null` clears it (falls back to the
/// first open folder). The root must be one of the open, synced workspace
/// folders — a path outside confinement is refused, never stored.
#[tauri::command]
pub async fn conductor_set_root(
    state: State<'_, AppState>,
    root: Option<String>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "conductor:cwd")?;
    let Some(root) = root else {
        state.conductor.set_cwd(None);
        return Ok(json!({}));
    };
    let root = root.trim().to_string();
    if root.is_empty() {
        state.conductor.set_cwd(None);
        return Ok(json!({}));
    }
    // Confinement: only an OPEN, SYNCED workspace folder may become the
    // assistant's root — the renderer is free text, and a path outside the
    // open folders must never steer tool cwd (the same rule read_file's
    // resolver applies).
    match crate::confine::confined_real_path(&state, std::path::Path::new(&root)) {
        Ok(confined) => {
            state
                .conductor
                .set_cwd(Some(confined.to_string_lossy().into_owned()));
            Ok(json!({}))
        }
        Err(reason) => Err(format!("conductor:cwd: {reason}")),
    }
}
