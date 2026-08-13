//! UI theme sync (fire-and-forget `{ pref, mode }` from the renderer).
//! Ports `src/main/index.js`'s `theme:set` handler; main uses the resolved
//! mode for window backgrounds and CSS injected into converted-document
//! iframes. `AppState.theme` already exists for this to write into.

use tauri::{AppHandle, Manager, State};

use crate::{lock_gate, state::AppState};

/// `src/main/index.js`'s `WINDOW_BG` constant.
fn window_bg(mode: &str) -> &'static str {
    if mode == "dark" {
        "#050508"
    } else {
        "#eeeef2"
    }
}

/// Mirrors `ipcMain.on('theme:set', (e, msg) => { ... })`: normalizes
/// `pref`/`mode` the same defensive way the JS handler does (`msg?.pref
/// === 'light' || msg?.pref === 'dark' ? msg.pref : 'system'`, `msg?.mode
/// === 'dark' ? 'dark' : 'light'`), stores the resolved payload in
/// `AppState.theme`, and repaints every window's background —
/// `BrowserWindow.getAllWindows().forEach(w => w.setBackgroundColor(...))`
/// there, `AppHandle::webview_windows()` here.
///
/// NOT ported: `nativeTheme.themeSource = pref`, which only steers native
/// OS chrome (title bar, context menus) to follow the preference — out of
/// scope per this slice's brief ("apply window background color if the JS
/// does"), and there is no direct Tauri equivalent for "make native window
/// chrome track an arbitrary light/dark/system preference" the way
/// Electron's `nativeTheme.themeSource` does.
#[tauri::command]
pub async fn theme_set(
    app: AppHandle,
    state: State<'_, AppState>,
    pref: Option<String>,
    mode: Option<String>,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "theme:set")?;
    let pref = match pref.as_deref() {
        Some("light") => "light",
        Some("dark") => "dark",
        _ => "system",
    };
    let mode = if mode.as_deref() == Some("dark") { "dark" } else { "light" };
    *state
        .theme
        .write()
        .expect("theme_set: AppState.theme lock poisoned") =
        serde_json::json!({ "pref": pref, "mode": mode });
    if let Ok(color) = window_bg(mode).parse::<tauri::window::Color>() {
        for (_, window) in app.webview_windows() {
            let _ = window.set_background_color(Some(color));
        }
    }
    Ok(serde_json::json!({}))
}
