//! Native folder/file picker commands, backed by `tauri-plugin-dialog`
//! (registered in `lib.rs`; permissions granted in
//! `capabilities/default.json`). Ports `src/main/index.js`'s
//! `dialog:pickFolder`/`dialog:pickFile` handlers.

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{lock_gate, state::AppState};

/// Mirrors `dialog.showOpenDialog(win, { properties: ['openDirectory'] });
/// r.canceled ? null : r.filePaths[0]`. `blocking_pick_folder` is safe to
/// call here despite its own docs warning it "should NOT be used when
/// running on the main thread": this command is `async`, and the plugin's
/// own example shows exactly this pattern (`async fn my_command(app:
/// AppHandle) { app.dialog().file().blocking_pick_folder() }`) — Tauri
/// commands run off the UI thread, so the block lands on a worker thread,
/// not the one the native dialog needs to pump events on.
///
/// Not ported: passing `win` as the dialog's parent (Electron's
/// `dialog.showOpenDialog(win, ...)` ties the sheet to the main window).
/// `tauri-plugin-dialog`'s builder can attach a parent explicitly, but
/// there is exactly one window in this app and no modal-stacking scenario
/// yet where the difference would be observable — noted as a minor gap
/// rather than worked around.
#[tauri::command]
pub async fn dialog_pick_folder(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "dialog:pickFolder")?;
    Ok(match app.dialog().file().blocking_pick_folder() {
        Some(path) => serde_json::json!(path.to_string()),
        None => serde_json::Value::Null,
    })
}

/// Same shape as `dialog_pick_folder`, for `dialog:pickFile` /
/// `dialog.showOpenDialog(win, { properties: ['openFile'] })`.
#[tauri::command]
pub async fn dialog_pick_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "dialog:pickFile")?;
    Ok(match app.dialog().file().blocking_pick_file() {
        Some(path) => serde_json::json!(path.to_string()),
        None => serde_json::Value::Null,
    })
}
