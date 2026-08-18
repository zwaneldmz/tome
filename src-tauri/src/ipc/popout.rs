//! Popout window close acknowledgement. A popped-out pane window asking to
//! close is vetoed (`lib.rs`'s `CloseRequested` handler) until the main
//! window's renderer answers its move-or-close prompt; this command is that
//! answer — "approved, let it go". Ports `src/main/index.js`'s
//! `ipcMain.handle('popout:close', ...)` (~1131): not calling this is how
//! "cancel" works — the window simply stays open.
//!
//! The `id` argument is the Tauri window LABEL of the popout (for example
//! `popout-dock-3`). Under Electron it was a numeric `BrowserWindow.id`;
//! Tauri has no numeric window ids, and labels are the lookup key
//! (`app.get_webview_window(label)`), so the renderer shim sends the label
//! it carries in the close-request event. The veto side (`lib.rs`) labels
//! every popout window it creates with the `popout` prefix
//! `capabilities/default.json`'s `windows: ["main", "popout*"]` already
//! scopes for.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::{lock_gate, state::AppState};

#[tauri::command]
pub async fn popout_close(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "popout:close")?;
    // Only ever a popout window — mirroring Electron's
    // `BrowserWindow.fromId(id)` + `popoutApproved.add(id)` + `close()`,
    // which could only ever have been armed with a popout child id in the
    // first place (`did-create-window` + `isPopoutUrl`). Refusing a
    // non-popout label here is the Tauri-side equivalent of that closed
    // set: without it, a compromised renderer could close the MAIN window
    // out from under the quit handshake by sending its label.
    if !id.starts_with("popout") {
        return Err(format!("popout:close: not a popout window: {id}"));
    }
    if let Some(window) = app.get_webview_window(&id) {
        // `close()` re-fires CloseRequested; lib.rs's handler consults the
        // approved-labels set this command just armed (via its own
        // `popout_approved` map on AppState) and lets this one through.
        state
            .popout_approved
            .lock()
            .expect("AppState.popout_approved lock poisoned")
            .insert(id.clone());
        window.close().map_err(|e| format!("popout:close: {e}"))?;
    }
    // A label with no live window is a no-op, matching the JS original's
    // `if (!child || child.isDestroyed()) return`.
    Ok(json!(null))
}
