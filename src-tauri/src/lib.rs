mod agent_env;
mod agent_spawn;
mod confine;
mod custom_agents;
mod events;
mod eventlog;
mod fs;
mod git;
mod ipc;
mod lock_gate;
mod login_env;
mod menu;
mod pty;
mod pty_authority;
mod state;
mod store;
mod store_keys;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{Emitter, Manager, WindowEvent};

use state::AppState;

/// Builds the plugin that injects `window.__TOME_BOOT__` before any page
/// script runs, on every window (including the config-declared main
/// window). Mirrors `src/preload/index.js`'s `home`/`shotMode`/`profile`
/// properties — those were computed at Electron preload time (i.e. before
/// the renderer's own script ran), which a plugin's `js_init_script` is the
/// Tauri equivalent of. `WebviewWindowBuilder::initialization_script`
/// would NOT reach the main window here, since that window comes from
/// `tauri.conf.json`, not a programmatic builder call.
fn boot_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let home = std::env::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `!!process.env.TOME_SHOT` in JS is true only for a SET and NON-EMPTY
    // value (JS treats "" as falsy) — matched exactly here rather than with
    // `.is_ok()`, which would also fire on `TOME_SHOT=`.
    let truthy_env = |name: &str| std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false);
    let boot = serde_json::json!({
        "home": home,
        "shotMode": truthy_env("TOME_SHOT"),
        "profile": truthy_env("TOME_PROFILE"),
    });
    tauri::plugin::Builder::new("tome-boot")
        .js_init_script(format!("window.__TOME_BOOT__ = {boot};"))
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Guards the quit handshake below against a second CloseRequested
    // arriving while an earlier one's handshake is still in flight —
    // mirrors index.js's module-level `quitting` flag. Not part of
    // AppState: it's purely an implementation detail of this closure, not
    // something any command needs to read.
    let quitting = AtomicBool::new(false);

    tauri::Builder::default()
        // Must be the first plugin registered — its own docs are explicit
        // that it needs to intercept a second launch before anything else
        // initializes.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.unminimize();
            }
        }))
        .plugin(boot_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            menu::setup(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // pty (Phase 2)
            ipc::pty::pty_create,
            ipc::pty::pty_write,
            ipc::pty::pty_resize,
            ipc::pty::pty_kill,
            // fs
            ipc::fs::fs_read_dir,
            ipc::fs::fs_read_file,
            ipc::fs::fs_write_file,
            ipc::fs::fs_mkdir,
            ipc::fs::fs_create_file,
            ipc::fs::fs_watch,
            ipc::fs::fs_unwatch,
            // fmt
            ipc::fmt::fmt_format,
            // store
            ipc::store::store_get,
            ipc::store::store_set,
            // git
            ipc::git::git_info,
            ipc::git::git_branches,
            ipc::git::git_checkout,
            ipc::git::git_log,
            ipc::git::git_commit,
            ipc::git::git_diff,
            // auth (Phase 3)
            ipc::auth::auth_status,
            ipc::auth::auth_login,
            ipc::auth::auth_touchid,
            // panes / workspace sync
            ipc::panes::panes_sync,
            ipc::panes::ws_sync,
            // conductor
            ipc::conductor::conductor_allow_run,
            ipc::conductor::conductor_allow_read,
            // doc
            ipc::doc::doc_read,
            // theme
            ipc::theme::theme_set,
            // shell
            ipc::shell::shell_open_path,
            // airgap (Phase 3/4)
            ipc::airgap::airgap_state,
            ipc::airgap::airgap_unlock,
            ipc::airgap::airgap_relock,
            ipc::airgap::airgap_setup,
            ipc::airgap::airgap_enroll_totp,
            ipc::airgap::airgap_confirm_totp,
            ipc::airgap::airgap_read_repo_allowlist,
            ipc::airgap::airgap_consent_repo_allowlist,
            ipc::airgap::airgap_revoke_repo_allowlist,
            // agents
            ipc::agents::agents_list,
            ipc::agents::agents_customs,
            ipc::agents::agents_changed,
            // events
            ipc::events::events_list,
            // runs (flows)
            ipc::runs::runs_start,
            ipc::runs::runs_cancel,
            ipc::runs::runs_list,
            // stt
            ipc::stt::stt_transcribe,
            ipc::stt::stt_warmup,
            ipc::stt::stt_status,
            // chat
            ipc::chat::chat_send,
            ipc::chat::chat_abort,
            ipc::chat::chat_providers,
            // brain
            ipc::brain::brain_open,
            ipc::brain::brain_close,
            ipc::brain::brain_index,
            ipc::brain::brain_read,
            ipc::brain::brain_write,
            ipc::brain::brain_delete,
            ipc::brain::brain_core_info,
            ipc::brain::brain_promote,
            // lsp
            ipc::lsp::lsp_did_open,
            ipc::lsp::lsp_did_change,
            ipc::lsp::lsp_did_close,
            ipc::lsp::lsp_hover,
            ipc::lsp::lsp_definition,
            // dialog
            ipc::dialog::dialog_pick_folder,
            ipc::dialog::dialog_pick_file,
            // app
            ipc::app::app_quit_ready,
            // popout
            ipc::popout::popout_close,
        ])
        // Quit handshake: give the renderer one beat to persist its dockview
        // layout before the process goes away. Ports index.js's
        // `app.on('before-quit', ...)` / `ipcMain.on('app:quit-ready', ...)`
        // pair (search index.js for "quit handshake").
        //
        // This fires for window-close (red button / Cmd+W) AND for the App
        // menu's "Quit" item / its Cmd+Q accelerator: menu.rs deliberately
        // does NOT wire those to `PredefinedMenuItem::quit` (AppKit's
        // `terminate:` selector skips `WindowEvent::CloseRequested`
        // entirely and lands on the non-cancelable `RunEvent::Exit`
        // instead, which nothing here handles — see menu.rs's top doc
        // comment for the empirical trace) and instead calls
        // `Window::close()`, which sends the same `WindowEvent::CloseRequested`
        // this closure is wired to.
        //
        // Electron's version guards re-entrant `before-quit` firing (from
        // its own `app.quit()` calls) with a `quitting` flag, then lets a
        // SECOND before-quit pass through unprevented so the retry actually
        // closes the app. Tauri has no equivalent re-entrancy: calling
        // `AppHandle::exit` below drives `RunEvent::ExitRequested` directly
        // rather than re-triggering window `CloseRequested`, so there is no
        // "let the retry through" case to build — instead, `quitting` here
        // simply avoids emitting `app:before-quit` and starting a second
        // 1.5s timer if the user requests a close twice before the first
        // handshake resolves (the second request's window is then allowed
        // to close immediately, which — this being the app's only window —
        // exits the app via Tauri's default all-windows-closed behavior).
        .on_window_event(move |window, event| {
            if window.label() != "main" {
                return;
            }
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            if quitting.swap(true, Ordering::SeqCst) {
                return;
            }
            api.prevent_close();
            let _ = window.emit("app:before-quit", ());
            let app_handle = window.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();
                // Hard cap: never hang the quit, matching index.js's
                // `setTimeout(() => app.quit(), 1500)`. `app_quit_ready`
                // (ipc::app) notifies this early when the renderer finishes
                // its persistence beat first.
                let _ = tokio::time::timeout(Duration::from_millis(1500), state.quit_ready.notified())
                    .await;
                app_handle.exit(0);
            });
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
