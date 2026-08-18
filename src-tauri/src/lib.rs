mod agent_env;
mod agent_spawn;
mod authlock;
mod brain;
mod chat;
mod conductor;
mod confine;
mod custom_agents;
pub mod egress;
mod eventlog;
mod events;
mod export;
mod flow;
// The tauri-touching half of `tome-flow`'s injected `RunnerEnv` seam — see
// this module's own doc comment for the split (plan step 2.1).
mod flow_env;
mod fs;
mod git;
mod ipc;
// Phase 4/slice L3: the real-bwrap curl-matrix proof — #[ignore]'d #[test]s
// that actually spawn tome-shim inside a real Linux network namespace. See
// that file's own doc comment for the full rationale, and
// .github/workflows/linux-sandbox.yml for the only place these ever run.
// `cfg(all(test, target_os = "linux"))` — BOTH conditions matter:
// `target_os = "linux"` means this module is not merely skipped at
// test-run time on macOS, it is never even parsed/compiled there, so this
// crate's native `cargo check`/`cargo test` gates can never be broken by
// anything in it; `test` means it is never pulled into a normal
// (non-test) build EVEN ON LINUX, which matters because the file uses
// `tempfile` — a `[dev-dependencies]`-only crate that a plain `cargo
// build`/`cargo check` of the real shipped binary does not link at all.
// Omitting the `test` half would make a real Linux release build fail to
// compile.
#[cfg(all(test, target_os = "linux"))]
mod linux_sandbox_integration_tests;
mod lock_gate;
mod login_env;
mod lsp;
mod mentor;
mod menu;
mod migrate;
mod protocol;
mod pty;
mod pty_authority;
mod remote;
mod review;
mod schedule;
mod skills;
mod state;
mod store;
mod store_keys;
mod stt;
mod totp;
mod touchid;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{Emitter, Manager, WindowEvent};

use state::AppState;

/// `!!process.env.<name>` in JS: true only for a SET and NON-EMPTY value
/// (JS treats `""` as falsy) — matched exactly here rather than with
/// `.is_ok()`, which would also fire on `TOME_SHOT=`. Shared by
/// [`boot_plugin`] (`shotMode`/`profile` boot flags) and [`run`]'s own
/// `shot_mode` computation for the initial lock-gate state (`index.js`'s
/// `const shotMode = !!process.env.TOME_SHOT && !app.isPackaged`).
fn truthy_env(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Builds the plugin that injects `window.__TOME_BOOT__` before any page
/// script runs, on every window (including the config-declared main
/// window). Mirrors `src/preload/index.js`'s `home`/`shotMode`/`profile`
/// properties — those were computed at Electron preload time (that is before
/// the renderer's own script ran), which a plugin's `js_init_script` is the
/// Tauri equivalent of. `WebviewWindowBuilder::initialization_script`
/// would NOT reach the main window here, since that window comes from
/// `tauri.conf.json`, not a programmatic builder call.
/// Popout window support — the Tauri replacement for Electron's
/// `setWindowOpenHandler` + `did-create-window` pair (`src/main/index.js`
/// ~411-437). dockview tears a pane group off with `window.open()` on
/// `popout.html`; wry has no same-context `window.open`, so Tauri routes
/// that navigation through this plugin's `window_created_with` hook, which
/// turns it into a real `WebviewWindow` (labeled `popout-*`, matching
/// `capabilities/default.json`'s window scope) and vetoes the navigation
/// in the original webview.
///
/// The window's own URL keeps the requested `popout.html` — dev mode
/// resolves it against the vite dev server, packaged mode against the
/// bundled frontend dist, exactly like the main window's own load. The
/// label carries the popout group's name through (dockview names each
/// popout window `${dockId}-${groupId}` and passes it as `window.open`'s
/// frameName, which arrives here in the navigation URL's query — see
/// `panes.js`'s `addPopoutGroup` call) so the close-request handshake can
/// map a window back to the panes inside it, the same job Electron's
/// `frameName` did.
fn popout_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("tome-popout")
        .on_navigation(|window, url| {
            // Only the main window can spawn a popout; a popout navigating
            // itself (its own initial load, or anything else) passes
            // through untouched.
            if window.label() != "main" {
                return true;
            }
            if !url.path().ends_with("/popout.html") {
                // Only popout navigations are intercepted; everything else
                // (the main window's own initial load, in-app hash
                // changes) passes through untouched. External links never
                // reach here at all: the renderer routes those through
                // `tome.shell.openExternal` → `tauri-plugin-opener`,
                // which opens the OS browser — the same split Electron's
                // `setWindowOpenHandler`/`shell.openExternal` pairing had.
                return true;
            }
            let app = window.app_handle().clone();
            let url = url.clone();
            // The window build must not happen inside the navigation
            // callback itself (wry re-entrancy) — defer one tick.
            tauri::async_runtime::spawn(async move {
                use tauri::{WebviewUrl, WebviewWindowBuilder};
                static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let label = format!("popout-{n}");
                // A real title bar, unlike the main window's hiddenTitle —
                // popout.html has no topbar to offer as a drag region, and
                // the bar leaves dockview's tab strip free as a drop
                // target for panes dragged in from another window (the JS
                // original's own overrideBrowserWindowOptions comment).
                // `WebviewUrl::External` keeps the navigation URL's
                // scheme/host intact — in dev that's the vite dev-server
                // URL the renderer resolved `popout.html` against; in a
                // packaged build it's the same `tauri://localhost` (macOS)
                // / `http://tauri.localhost` (Linux/Windows) origin the
                // main window itself loaded from, so the popout document
                // stays same-origin with it (dockview's DOM move requires
                // that).
                if let Err(e) = WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(url))
                    .title("Tome")
                    .min_inner_size(320.0, 200.0)
                    .inner_size(940.0, 640.0)
                    .build()
                {
                    eprintln!("[tome-popout] failed to create popout window: {e}");
                }
            });
            // Always veto the in-webview navigation — the popout lives in
            // its own window now.
            false
        })
        .build()
}

fn boot_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let home = std::env::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let boot = serde_json::json!({
        "home": home,
        "shotMode": truthy_env("TOME_SHOT"),
        "profile": truthy_env("TOME_PROFILE"),
    });
    tauri::plugin::Builder::new("tome-boot")
        .js_init_script(format!("window.__TOME_BOOT__ = {boot};"))
        .build()
}

/// Boot-time load: `authlock::AuthLock::load` (the passphrase/TOTP store)
/// and `egress::EgressState::load_repo_consents` (persisted repo-allowlist
/// consents), both off `app_data_dir` — Tauri's per-OS analog of
/// Electron's `app.getPath('userData')`, which is what both
/// `authlock.initAuth(userData)` and `egress.loadRepoConsents(userData)`
/// receive at their one real call site in `index.js` (~548-552). Also
/// computes this process's initial lock-gate state (`index.js`'s
/// `isLockedNow()`) once — see `lock_gate::is_locked`'s doc comment for why
/// a one-time boot computation is equivalent to the JS original's
/// recompute-on-every-call version, given how `AppState.locked` is
/// maintained afterward.
///
/// Not itself fallible in any way that should abort `.setup()`:
/// `AuthLock::load`/`load_repo_consents` both already collapse every
/// failure mode (missing file, corrupt JSON) to "start fresh/empty" — see
/// their own doc comments — matching `initAuth`'s `catch { auth = null }` /
/// `loadRepoConsents`'s `catch {}`. `app_data_dir` itself failing to
/// resolve is the one real failure this function can hit; the same
/// fallback every other command's own `app.path().app_data_dir()` call
/// already has to tolerate (see `ipc::pty::pty_create`/`ipc::store::get`'s
/// call sites) — boot must not panic `.setup()` over it, so this simply
/// leaves `AppState.auth` at its starting `None` and the app boots
/// unlocked (`AppState.locked` stays `false`, its `AppState::new()`
/// default) rather than crashing.
fn boot_auth_and_egress<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    // Tauri, unlike Electron's `userData` (which Electron itself creates
    // before `whenReady` fires), does not guarantee `app_data_dir` exists —
    // same discipline `store::set`/`events::append` already apply to their
    // own writes under this directory.
    let _ = std::fs::create_dir_all(&dir);

    let auth = authlock::AuthLock::load(&dir);
    let shot_mode = truthy_env("TOME_SHOT") && tauri::is_dev();
    let locked = lock_gate::is_locked(auth.status().configured, false, shot_mode);

    let state = app.state::<AppState>();
    *state.locked.write().expect("AppState.locked lock poisoned") = locked;
    *state.auth.lock().expect("AppState.auth lock poisoned") = Some(auth);
    state
        .egress
        .load_repo_consents(&dir.join("egress-repo-consents.json"));
}

/// Spawns the in-app scheduler's 30-second tick loop (plan §Flow products
/// pipeline step 1.7) and stores its `AbortHandle` on `AppState.
/// schedule_ticker` so the quit handshake below (`abort_schedule_ticker`)
/// can cancel it. Called once from `.setup()`, after `boot_auth_and_egress`
/// — order does not matter functionally (the ticker only ever reads
/// `AppState.locked`/`app_data_dir` per tick, never anything
/// `boot_auth_and_egress` seeds once at startup), but keeping every
/// boot-time background task's spawn call in one place, right after the
/// other one, is easier to audit than scattering them through `.setup()`.
///
/// `tokio::time::interval`'s default first tick fires immediately (not
/// after the first 30s) — deliberately left as-is rather than skipped: a
/// schedule that has never run is due the moment anything checks (see
/// `schedule::next_due`'s doc comment), so there is no reason to make a
/// fresh install wait out a whole extra period before its first schedule
/// can fire.
fn spawn_schedule_ticker(app: &tauri::AppHandle) {
    let app_handle = app.clone();
    let join = tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            ipc::schedules::run_tick(&app_handle).await;
        }
    });
    let state = app.state::<AppState>();
    *state
        .schedule_ticker
        .lock()
        .expect("AppState.schedule_ticker lock poisoned") = Some(join.inner().abort_handle());
}

/// Cancels the scheduler's tick loop — the quit-time counterpart to
/// [`spawn_schedule_ticker`], called from the `CloseRequested` handler right
/// alongside `shutdown_all_proxies` (that function's own relock-timer drain
/// is "the existing timer drain" this one is deliberately kept next to,
/// rather than folded INTO a function named for proxies specifically). A
/// scheduled run's own already-spawned child processes are reaped
/// separately by `flow::runner::kill_all` (called right after, in `run()`'s
/// quit path) regardless — this function's only job is making sure the
/// ticker itself stops POLLING once the app is on its way down, rather than
/// firing `flow::runner::start_run` into a process that is mid-exit.
/// Idempotent: `Option::take` leaves nothing for a second call to abort.
fn abort_schedule_ticker(state: &AppState) {
    if let Some(handle) = state
        .schedule_ticker
        .lock()
        .expect("AppState.schedule_ticker lock poisoned")
        .take()
    {
        handle.abort();
    }
}

/// Shuts down every live pane proxy (loopback listener + any established
/// tunnels) and cancels every pending auto-relock timer — the quit-time
/// half of `closeAll()` (`egress.js`'s own doc comment: "proxies are
/// children of no window", so nothing else tears them down when the app
/// exits). Idempotent, like the JS original (`will-quit` and
/// `window-all-closed` both call `closeAll()` there; this crate's own quit
/// path only calls this once, from the `CloseRequested` handler below, but
/// it would be harmless to call twice — draining a map a second time finds
/// nothing left). No `egress:state` push here, matching `closeAll`'s own
/// comment: the window is on its way down, and a locked/closing app has
/// nowhere left to deliver the event.
fn shutdown_all_proxies(state: &AppState) {
    for (_, (_, timer)) in state
        .relock_timers
        .lock()
        .expect("AppState.relock_timers lock poisoned")
        .drain()
    {
        timer.abort();
    }
    for (_, proxy) in state
        .proxies
        .lock()
        .expect("AppState.proxies lock poisoned")
        .drain()
    {
        proxy.shutdown();
    }
    state.egress.close_all();
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
        .plugin(popout_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        // tome:// custom protocol — serves confined, extension-allowlisted
        // file bytes to the sandboxed doc-viewer iframe
        // (`src/renderer/panels/doc.js`). Ports index.js's privileged
        // `tome` scheme registration (~line 258) and its
        // `protocol.handle('tome', ...)` handler (~line 528); registered
        // globally here exactly like the Electron original, so it needs no
        // separate wiring for the (currently flagged-off) popout webview
        // once that lands. See `protocol.rs`'s module doc comment for the
        // full port notes, including why `tauri.conf.json` needed no new
        // scheme declaration. Closure-wrapped (rather than passing
        // `protocol::handle` directly) so the compiler resolves this
        // builder's runtime type `R` before monomorphizing the generic
        // handler, avoiding an inference dead end at this call site.
        .register_asynchronous_uri_scheme_protocol("tome", |ctx, request, responder| {
            protocol::handle(ctx, request, responder)
        })
        .setup(|app| {
            menu::setup(app)?;
            // Best-effort first-boot copy of a legacy Electron userData
            // profile into this build's own app_data_dir. MUST run before
            // boot_auth_and_egress below, so a freshly-migrated
            // egress-auth.json/egress-repo-consents.json (if any) is what
            // that call's own AuthLock::load/load_repo_consents sees on
            // this same boot rather than the next one. See migrate.rs's
            // module doc comment.
            migrate::run(app.handle());
            boot_auth_and_egress(app.handle());
            spawn_schedule_ticker(app.handle());
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
            ipc::git::git_status,
            ipc::git::git_stage,
            ipc::git::git_commit_create,
            ipc::git::git_push,
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
            ipc::doc::doc_read_bytes,
            // theme
            ipc::theme::theme_set,
            // shell
            ipc::shell::shell_open_path,
            // egress (Phase 3/4)
            ipc::egress::egress_state,
            ipc::egress::egress_unlock,
            ipc::egress::egress_relock,
            ipc::egress::egress_setup,
            ipc::egress::egress_enroll_totp,
            ipc::egress::egress_confirm_totp,
            ipc::egress::egress_read_repo_allowlist,
            ipc::egress::egress_consent_repo_allowlist,
            ipc::egress::egress_revoke_repo_allowlist,
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
            // export destinations (and the runs-pane Export action)
            ipc::export::export_destinations,
            ipc::export::export_consent,
            ipc::export::export_revoke,
            ipc::export::runs_export,
            // schedules (in-app scheduler)
            ipc::schedules::schedules_list,
            ipc::schedules::schedules_set,
            ipc::schedules::schedules_delete,
            // remote run visibility (plan phase 3)
            ipc::remote::remote_sources,
            ipc::remote::remote_consent,
            ipc::remote::remote_revoke,
            ipc::remote::remote_runs,
            ipc::remote::remote_run_detail,
            // stt
            ipc::stt::stt_engine,
            ipc::stt::stt_transcribe,
            ipc::stt::stt_warmup,
            ipc::stt::stt_status,
            // chat
            ipc::chat::chat_send,
            ipc::chat::chat_abort,
            ipc::chat::chat_providers,
            ipc::chat::chat_complete,
            // mentor
            ipc::mentor::mentor_answer,
            ipc::mentor::mentor_judge,
            // review
            ipc::review::review_generate,
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
            // skills
            ipc::skills::skills_list,
            ipc::skills::skills_read,
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
            // ---- popout windows: veto-first close handshake ----
            // Ports `watchPopout` (`src/main/index.js` ~375-385): a popout
            // asking to close is held open until the MAIN window's
            // renderer answers its move-or-close prompt with
            // `popout:close` (`ipc::popout::popout_close`), which arms the
            // label in `AppState.popout_approved` — never arming it is how
            // "cancel" works. Never veto during a quit (`quitting`), or
            // once the main window is gone — there would be nothing left
            // to show the prompt (the JS original's own `win.isDestroyed()`
            // guard).
            if window.label() != "main" {
                let label = window.label().to_string();
                match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        if !label.starts_with("popout") || quitting.load(Ordering::SeqCst) {
                            return;
                        }
                        let state = window.app_handle().state::<AppState>();
                        let mut approved = state
                            .popout_approved
                            .lock()
                            .expect("AppState.popout_approved lock poisoned");
                        if approved.remove(&label) {
                            return; // renderer-approved: let it close
                        }
                        drop(approved);
                        let Some(main) = window.app_handle().get_webview_window("main") else {
                            return; // no main window left to prompt in
                        };
                        api.prevent_close();
                        // Same payload shape Electron sent
                        // (`win.webContents.send('popout:close-request',
                        // { id, name })`) — `id` is the window label here
                        // (Tauri has no numeric window ids), `name` is the
                        // same label: it uniquely names the popout group
                        // for the renderer's `dock.groups` lookup, the job
                        // Electron's `frameName` did.
                        let _ = main.emit(
                            "popout:close-request",
                            serde_json::json!({ "id": label, "name": label }),
                        );
                    }
                    WindowEvent::Destroyed
                        // Mirrors `child.on('closed', () =>
                        // popoutApproved.delete(child.id))` — a stale armed
                        // label must not outlive its window (labels are
                        // counter-unique, so reuse is impossible, but the
                        // set would grow without bound otherwise).
                        if label.starts_with("popout") => {
                            let state = window.app_handle().state::<AppState>();
                            state
                                .popout_approved
                                .lock()
                                .expect("AppState.popout_approved lock poisoned")
                                .remove(&label);
                        }
                    _ => {}
                }
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
                let _ =
                    tokio::time::timeout(Duration::from_millis(1500), state.quit_ready.notified())
                        .await;
                // Extend index.js's exit cleanup: pane proxies are children
                // of no window (see `shutdown_all_proxies`'s doc comment) —
                // without this, a proxy from a spawn that never got a
                // matching `pty:kill` (or one still `Open` mid-unlock-window)
                // would keep its loopback port bound, and any of its live
                // tunnels would keep piping bytes, past process exit.
                shutdown_all_proxies(&state);
                abort_schedule_ticker(&state);
                // Matches index.js's `will-quit`/`window-all-closed` both
                // calling `flowRunner.killAll()` — a background flow run's
                // headless node processes are their own process-group
                // leaders (see `flow::runner::spawn`'s doc comment), so
                // nothing signals them when this process exits on its own;
                // left unwired, they would outlive the app entirely,
                // orphaned with no window and no way left to cancel them.
                // Idempotent, like `shutdown_all_proxies` above.
                flow::runner::kill_all(&state.flow);
                // Matches index.js's `lsp.shutdownAll()` quit-path call —
                // language servers are child processes of no window either;
                // `lsp::shutdown_all`'s own doc comment covers why this stays
                // fast enough for the 1.5s cap above.
                lsp::shutdown_all().await;
                app_handle.exit(0);
            });
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
