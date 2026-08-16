//! Native application menu — full port of `buildMenu()`
//! (`src/main/index.js`, roughly lines 1335-1517). Every custom item emits
//! the exact same `{id, ...}` shape on the `"menu:action"` event the
//! Electron original sent via `win.webContents.send('menu:action', action)`,
//! so `src/renderer/menu-bridge.js` needs no changes to consume it.
//!
//! Deviations from the Electron original, most consequential first:
//!
//! - Built on every platform. Electron's `buildMenu()` opened with `if
//!   (process.platform !== 'darwin') return` — no menu at all off macOS.
//!   `tauri::App::set_menu` renders the SAME template as the global menu
//!   bar on macOS and a per-window menu bar on Linux/Windows automatically,
//!   so withholding it elsewhere would be a regression here, not parity.
//! - The App menu's "Quit" item is a plain custom `MenuItem` (id `"quit"`,
//!   accelerator `CmdOrCtrl+Q`), NOT `PredefinedMenuItem::quit`. This was
//!   verified empirically (reading the vendored muda/tao/tauri-runtime-wry
//!   sources this crate actually resolves to) to be load-bearing, not
//!   cosmetic: `PredefinedMenuItemType::Quit` binds to AppKit's `terminate:`
//!   selector (muda-0.19.3 `platform_impl/macos/mod.rs:994`), which — for
//!   an app with no `applicationShouldTerminate:` override (tao-0.35.3's
//!   app delegate implements only `applicationWillTerminate:`, confirmed by
//!   reading `platform_impl/macos/app_delegate.rs`) — skips
//!   `windowShouldClose:` entirely and so never produces
//!   `WindowEvent::CloseRequested`. It instead reaches tao's
//!   `AppState::exit()` (`applicationWillTerminate:`'s handler), which
//!   synchronously posts `Event::LoopDestroyed` — mapped by
//!   tauri-runtime-wry-2.11.4 (`src/lib.rs:4185-4186`) to `RunEvent::Exit`,
//!   a *different, non-cancelable* event that `lib.rs`'s
//!   `.run(tauri::generate_context!())` never handles (that shorthand
//!   installs a no-op `|_, _| {}` callback per tauri-2.11.5
//!   `src/app.rs:2449`). Worse, `RunEvent::Exit` fires synchronously on the
//!   main thread from inside `applicationWillTerminate:`, milliseconds
//!   before AppKit tears the process down — there is no reliable way to
//!   `await` the renderer's before-quit round trip from there (an
//!   `async_runtime::spawn`'d task racing the teardown is not a fix, just a
//!   flaky one), so the correct fix is to never let AppKit's native
//!   `terminate:` own this path at all. This custom item's handler (see
//!   `setup()` below) instead resolves the main `Window<R>` and calls
//!   `Window::close()` on it — confirmed (`tauri-runtime-wry-2.11.4`) to
//!   send `WindowMessage::Close`, which *does* route through
//!   `WindowEvent::CloseRequested`, reusing `lib.rs`'s existing, working
//!   quit handshake for both this menu item and the Cmd+Q accelerator it
//!   carries. (Two API notes on how it gets there: (1) it's
//!   `Manager::get_webview_window` + `AsRef<Webview<_>>` +
//!   `Webview::window()`, NOT `Manager::get_window` directly — the latter
//!   is gated behind Tauri's `unstable` cargo feature, which
//!   `Cargo.toml`'s `tauri = { version = "2", features = [] }` does not
//!   enable. (2) Calling `.close()` on the `WebviewWindow` itself instead
//!   (skipping the `Webview::window()` step) would NOT work — confirmed by
//!   reading its impl: that sends a *webview*-level `WebviewMessage::Close`,
//!   which just detaches the webview from the window and leaves the native
//!   window — and the app — open.) Residual gap, not closed by this: macOS
//!   Apple-Event-driven quit (Activity
//!   Monitor's "Quit" — not "Force Quit" — `osascript ... quit`, etc.)
//!   still resolves to `-[NSApplication terminate:]` outside this menu
//!   entirely, and would need an `applicationShouldTerminate:` delegate
//!   override upstream in tao to close; out of scope for this crate.
//! - The Appearance submenu's three items are plain `MenuItem`s, not a
//!   native checked/radio group. `menu-bridge.js`'s `'set-theme'` case
//!   never reads the action's `pref` at all — it always opens the same
//!   live picker the topbar ☾/☀ button uses (its own comment: "The native
//!   Appearance submenu can't render live radio state") — so a checked
//!   native item would add native-menu complexity for a checkmark nothing
//!   downstream observes. This also sidesteps the fact that Electron's own
//!   `checked: uiTheme === 'light'` is itself a one-shot snapshot
//!   (`buildMenu()` runs once at window creation, never rebuilt on theme
//!   change) — there was no live state to match in the first place.
//! - `role: 'reload'` / `'toggleDevTools'` (dev-build-only in the
//!   original, gated on `!app.isPackaged`) and `'resetZoom'` / `'zoomIn'`
//!   / `'zoomOut'` are Electron webContents features with no Tauri menu
//!   equivalent — skipped outright rather than approximated. The View
//!   menu's trailing separator before Toggle Fullscreen is kept singular
//!   rather than doubled (the JS had one on each side of the now-removed
//!   zoom trio).
//!
//! Native item ids double as the wire action id, `|`-joined with a
//! parameter for the two actions that carry one (`new-pane`, `set-theme`);
//! `action_payload` below is the one place that splits it back apart into
//! `menu-bridge.js`'s expected `{id, kind}` / `{id, pref}` shape.

use tauri::menu::{MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::ipc::agents::AGENTS;

/// Shorthand for `MenuItem::with_id(m, id, text, true, accel)` — every
/// custom (non-predefined) item in this file is enabled and built the same
/// way, differing only in id/text/accelerator.
fn item<R: Runtime, M: tauri::Manager<R>>(
    m: &M,
    id: &str,
    text: &str,
    accel: Option<&str>,
) -> tauri::Result<MenuItem<R>> {
    MenuItem::with_id(m, id, text, true, accel)
}

fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    // `app.package_info().name` resolves to `tauri.conf.json`'s
    // `productName` ("Tome") when set, which it is — see
    // `tauri-codegen`'s `context.rs`, `package_name` — so this matches
    // Electron's `label: app.name` after `app.setName('Tome')` exactly,
    // without hardcoding the string a second place.
    let name = app.package_info().name.clone();
    // Deliberately NOT `.quit()` (`PredefinedMenuItemType::Quit`, bound to
    // AppKit's `terminate:`) — see this file's top doc comment for why that
    // selector bypasses the quit handshake entirely. This plain item
    // carries the same label/accelerator convention but is routed through
    // `window.close()` in `setup()`'s `on_menu_event`, below.
    let quit = item(
        app,
        "quit",
        format!("Quit {name}").trim(),
        Some("CmdOrCtrl+Q"),
    )?;
    SubmenuBuilder::new(app, name)
        .about(None)
        .separator()
        .item(&item(
            app,
            "open-preferences",
            "Settings…",
            Some("CmdOrCtrl+,"),
        )?)
        .item(&item(app, "open-onboarding", "Setup Wizard…", None)?)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .item(&quit)
        .build()
}

fn file_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    SubmenuBuilder::new(app, "File")
        .item(&item(app, "open-file", "Open File…", Some("CmdOrCtrl+O"))?)
        .item(&item(
            app,
            "open-folder",
            "Open Folder in Workspace…",
            Some("CmdOrCtrl+Shift+O"),
        )?)
        .item(&item(app, "new-file", "New File…", Some("CmdOrCtrl+N"))?)
        .separator()
        .item(&item(app, "new-workspace", "New Workspace…", None)?)
        .separator()
        .item(&item(app, "save", "Save", Some("CmdOrCtrl+S"))?)
        .item(&item(app, "save-all", "Save All", Some("CmdOrCtrl+Alt+S"))?)
        .build()
}

fn edit_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()
}

fn view_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let appearance = SubmenuBuilder::new(app, "Appearance")
        .item(&item(app, "set-theme|light", "Light", None)?)
        .item(&item(app, "set-theme|dark", "Dark", None)?)
        .item(&item(app, "set-theme|system", "Match System", None)?)
        .build()?;
    SubmenuBuilder::new(app, "View")
        .item(&item(
            app,
            "toggle-sidebar",
            "Toggle Sidebar",
            Some("CmdOrCtrl+B"),
        )?)
        .item(&appearance)
        .separator()
        .item(&item(app, "quick-open", "Quick Open", Some("CmdOrCtrl+P"))?)
        .item(&item(
            app,
            "shortcuts",
            "Keyboard Shortcuts",
            Some("CmdOrCtrl+/"),
        )?)
        .item(&item(
            app,
            "toggle-voice",
            "Voice chat",
            Some("CmdOrCtrl+Shift+V"),
        )?)
        .separator()
        .fullscreen()
        .build()
}

fn pane_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    let mut new_pane = SubmenuBuilder::new(app, "New Pane")
        .item(&item(app, "new-pane|terminal", "Terminal", None)?)
        .item(&item(app, "new-pane|chat", "Assistant Chat", None)?)
        .item(&item(app, "new-pane|brain", "Brain", None)?)
        .separator();
    for &name in AGENTS {
        new_pane = new_pane.item(&item(app, &format!("new-pane|{name}"), name, None)?);
    }
    let new_pane = new_pane.build()?;
    SubmenuBuilder::new(app, "Pane")
        .item(&new_pane)
        .separator()
        .item(&item(app, "close-pane", "Close Pane", Some("CmdOrCtrl+W"))?)
        .build()
}

fn window_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Submenu<R>> {
    SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .close_window()
        .separator()
        .bring_all_to_front()
        .build()
}

/// Rebuilds `menu-bridge.js`'s exact `{id, ...}` payload from a native
/// item's id, splitting the one `|`-joined parameter back out for the two
/// actions that carry one. Returns `None` for anything `menu-bridge.js`'s
/// `switch (action?.id)` has no case for — every predefined/native item
/// (about, quit, copy, …) included, matching the Electron original: those
/// roles never called `send(...)` either, so no `menu:action` event for
/// them existed to receive in the first place.
fn action_payload(raw: &str) -> Option<serde_json::Value> {
    let (id, param) = match raw.split_once('|') {
        Some((id, param)) => (id, Some(param)),
        None => (raw, None),
    };
    match (id, param) {
        ("new-pane", Some(kind)) => Some(serde_json::json!({ "id": "new-pane", "kind": kind })),
        ("set-theme", Some(pref)) => Some(serde_json::json!({ "id": "set-theme", "pref": pref })),
        (
            "open-preferences" | "open-onboarding" | "toggle-sidebar" | "toggle-voice"
            | "quick-open" | "shortcuts" | "close-pane" | "save" | "save-all" | "open-file"
            | "open-folder" | "new-file" | "new-workspace",
            None,
        ) => Some(serde_json::json!({ "id": id })),
        _ => None,
    }
}

/// Called once from `lib.rs`'s `.setup()` hook. Builds the same template
/// `buildMenu()` did, installs it as the app-wide menu, and wires a single
/// `on_menu_event` listener that re-emits `"menu:action"` in the exact
/// shape `menu-bridge.js` already expects.
pub fn setup<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let handle = app.handle();
    let menu = MenuBuilder::new(app)
        .item(&app_menu(handle)?)
        .item(&file_menu(handle)?)
        .item(&edit_menu(handle)?)
        .item(&view_menu(handle)?)
        .item(&pane_menu(handle)?)
        .item(&window_menu(handle)?)
        .build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app_handle, event| {
        let id = event.id().as_ref();
        if id == "quit" {
            // Route through the SAME `WindowEvent::CloseRequested` path the
            // native close button uses, rather than AppKit's `terminate:`
            // — see this file's top doc comment for why that matters.
            // `.as_ref().window()` (not calling `.close()` on the
            // `WebviewWindow` itself): `Window::close()` sends the
            // window-level close the handshake in `lib.rs` is wired to,
            // where `WebviewWindow::close()` would send a webview-level
            // close that never reaches it. Goes via `get_webview_window`
            // rather than `Manager::get_window` since the latter needs
            // Tauri's `unstable` cargo feature, which this crate doesn't
            // enable.
            if let Some(webview_window) = app_handle.get_webview_window("main") {
                let _ = webview_window.as_ref().window().close();
            }
            return;
        }
        if let Some(payload) = action_payload(id) {
            let _ = app_handle.emit("menu:action", payload);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_payload_reconstructs_new_pane_shape() {
        assert_eq!(
            action_payload("new-pane|claude"),
            Some(serde_json::json!({ "id": "new-pane", "kind": "claude" }))
        );
    }

    #[test]
    fn action_payload_reconstructs_set_theme_shape() {
        assert_eq!(
            action_payload("set-theme|dark"),
            Some(serde_json::json!({ "id": "set-theme", "pref": "dark" }))
        );
    }

    #[test]
    fn action_payload_reconstructs_plain_ids() {
        assert_eq!(
            action_payload("close-pane"),
            Some(serde_json::json!({ "id": "close-pane" }))
        );
        assert_eq!(
            action_payload("save-all"),
            Some(serde_json::json!({ "id": "save-all" }))
        );
    }

    #[test]
    fn action_payload_ignores_predefined_and_unknown_ids() {
        // Predefined items (about/copy/…) get muda-assigned ids this
        // function never special-cases — same silence as the Electron
        // original, which never called `send(...)` for a `role:` item.
        assert_eq!(action_payload("1002"), None);
        assert_eq!(action_payload("MuDa-copy"), None);
    }

    #[test]
    fn action_payload_ignores_quit() {
        // "quit" is a real (non-predefined) custom item id — see
        // `app_menu()` — but `on_menu_event` special-cases and returns
        // before ever handing it to `action_payload`, so this pins the
        // fallback behavior defensively: even if that ordering ever
        // changed, no spurious `menu:action` should fire for it, matching
        // the Electron original's `role: 'quit'` never calling `send(...)`.
        assert_eq!(action_payload("quit"), None);
    }
}
