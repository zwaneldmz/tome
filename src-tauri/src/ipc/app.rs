//! App-lifecycle commands. Unlike its siblings, `app_quit_ready` is real
//! even in Phase 1: it is one half of the quit handshake installed on the
//! main window in `lib.rs::run()` (the other half is the `CloseRequested`
//! handler there), so it can't be left as `Err("unimplemented")` without
//! quietly breaking the very feature this slice was asked to build.

use tauri::State;

use crate::{lock_gate, state::AppState};

/// The renderer has finished its before-quit persistence (dockview layout,
/// etc.) and is ready for the process to exit. Wakes the `CloseRequested`
/// handler in `lib.rs`, which otherwise waits out a 1.5s hard cap before
/// exiting anyway — see `src/main/index.js`'s
/// `ipcMain.on('app:quit-ready', ...)` for the Electron original this
/// mirrors.
#[tauri::command]
pub async fn app_quit_ready(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "app:quit-ready")?;
    state.quit_ready.notify_one();
    Ok(serde_json::json!({}))
}
