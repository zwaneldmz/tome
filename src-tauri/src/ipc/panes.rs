//! Fire-and-forget state syncs from the renderer. `panes_sync` (pane list,
//! for the conductor) and `ws_sync` (open workspace folders, for
//! `crate::confine`'s root set) are both single-shot "sync renderer state
//! into main" commands with no dedicated Electron module of their own
//! (`src/main/index.js` handles both inline) — grouped here together rather
//! than getting one file each, since the command-surface brief did not list
//! a separate `ws` domain file.

use std::path::PathBuf;

use tauri::State;

use crate::{confine, ipc::airgap::recompile_all_proxies, lock_gate, state::AppState};

/// Mirrors `src/main/conductor.js`'s module-level `let panes = []` — the
/// renderer's pane snapshot (`[{id, title}, ...]`), synced fire-and-forget
/// on every pane open/close/rename via `ipcMain.on('panes:sync', (e, list)
/// => conductor.setPanes(list))`. Delegates straight to
/// `state.conductor.set_panes` (`conductor::Conductor::set_panes` mirrors
/// `setPanes`'s own `Array.isArray(list) ? list : []` guard) — this used to
/// keep its own local static here before `conductor::Conductor` existed to
/// fold it into; see that module's doc comment for the state it now lives
/// alongside (pane meta, scrollback, read consent).
#[tauri::command]
pub async fn panes_sync(
    state: State<'_, AppState>,
    list: serde_json::Value,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "panes:sync")?;
    state.conductor.set_panes(list);
    Ok(serde_json::json!({}))
}

/// Mirrors `index.js`'s `setOpenFolders` (called from `ipcMain.on('ws:sync',
/// ...)`):
///
/// ```js
/// function setOpenFolders(list) {
///   openFolders = Array.isArray(list) ? list.filter((f) => typeof f === 'string' && f) : []
///   foldersSynced = true
/// }
/// ```
///
/// keeping only non-empty string entries exactly like the JS's
/// `typeof f === 'string' && f` filter, and flipping `folders_synced` so
/// `confine::confined_real_path` (and this crate's `shell_open_path`) stop
/// refusing "not reported yet" the moment any sync — even an empty one —
/// has landed. Paths are stored raw, not `resolve()`d: the JS handler
/// doesn't normalize here either (only `isConfinedPath` calls `resolve()`,
/// at check time).
///
/// Also ports the JS handler's trailing `airgap.reapplyRepoConsents()`
/// call, PLUS the extra step this port's split between `airgap::AirgapState`
/// (pure bookkeeping) and the live `airgap::proxy::PaneProxy` per pane makes
/// necessary: `AirgapState::reapply_repo_consents` re-validates every
/// STORED consent (loaded at boot by `load_repo_consents`, before any
/// folder was known — confined resolution refuses until `folders_synced`
/// is true, which is exactly why this can only run for real once the FIRST
/// sync lands, matching the JS original's own comment on this ordering)
/// against the live file it pins, re-applying `applied_repos` for any that
/// still match. In the JS original that's the WHOLE story: `recompile()`
/// mutates the one shared `allowMatchers` every pane's `hostAllowed` reads
/// directly. Here, each `PaneProxy` holds its own independently-compiled
/// allow set (see `airgap::proxy`'s doc comment), so
/// [`recompile_all_proxies`] is the separate, additional step that pushes
/// the reapplied bookkeeping into every LIVE pane's actual enforcement.
/// Skipping it would leave `airgap:readRepoAllowlist` reporting
/// `consented: true` (from `AirgapState.repo_consents` alone, so the
/// renderer never re-prompts) while every gapped pane's proxy kept
/// blocking that host regardless — silently, on every single restart.
#[tauri::command]
pub async fn ws_sync(
    state: State<'_, AppState>,
    folders: serde_json::Value,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "ws:sync")?;
    let list: Vec<PathBuf> = match folders {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) if !s.is_empty() => Some(PathBuf::from(s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    *state
        .open_folders
        .write()
        .expect("ws_sync: AppState.open_folders lock poisoned") = list;
    *state
        .folders_synced
        .write()
        .expect("ws_sync: AppState.folders_synced lock poisoned") = true;

    state.airgap.reapply_repo_consents(|p| confine::confined_real_path(&state, p).ok());
    recompile_all_proxies(&state);

    Ok(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::airgap::effective_allow_patterns;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Regression test for the missing `ws_sync` -> `reapply_repo_consents`
    /// (+ `recompile_all_proxies`) wiring: a repo consent saved in one
    /// session must, after a fresh session's boot-time
    /// `AirgapState::load_repo_consents` followed by the SAME two calls
    /// `ws_sync` now makes, actually take effect on a LIVE `PaneProxy` —
    /// not just in `AirgapState`'s own bookkeeping. Exercises those two
    /// calls directly rather than through the `#[tauri::command]` wrapper
    /// itself, which needs a live `tauri::State` (see `events.rs`'s doc
    /// comment on this crate's established testing boundary for
    /// `AppHandle`-touching entry points — `ws_sync` only takes `State`,
    /// not `AppHandle`, but the same "needs a running app" constraint
    /// applies to constructing one at all outside of it).
    #[tokio::test]
    async fn reapplying_a_boot_loaded_consent_widens_a_live_proxys_enforcement() {
        let dir = tempfile::tempdir().unwrap();
        let tome_dir = dir.path().join(".tome");
        std::fs::create_dir_all(&tome_dir).unwrap();
        let allowlist_file = tome_dir.join("airgap.json");
        std::fs::write(&allowlist_file, r#"{"allow":["127.0.0.1"]}"#).unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let resolve = {
            let allowlist_file = allowlist_file.clone();
            move |_p: &std::path::Path| Some(allowlist_file.clone())
        };
        let consents_path = dir.path().join("airgap-repo-consents.json");

        // Session 1: user consents; persisted to disk.
        {
            let state = AppState::new();
            state.airgap.load_repo_consents(&consents_path);
            let report = state.airgap.read_repo_allowlist(&root, &resolve);
            let crate::airgap::RepoAllowlistReport::Present { hash, .. } = report else {
                panic!("expected Present")
            };
            let outcome = state.airgap.consent_repo_allowlist(&root, &hash, &resolve);
            assert!(matches!(outcome, crate::airgap::ConsentOutcome::Ok { .. }));
        }

        // Session 2 ("restart"): boot only ever calls load_repo_consents
        // (mirrors lib.rs's boot_auth_and_airgap) — applied_repos starts
        // empty even though repo_consents is loaded from disk.
        let state = AppState::new();
        state.airgap.load_repo_consents(&consents_path);
        assert!(
            !effective_allow_patterns(&state).contains(&"127.0.0.1".to_string()),
            "must not be applied yet — only reapply_repo_consents does that"
        );

        let (echo_port, _echo) = spawn_test_echo_server().await;
        let proxy = std::sync::Arc::new(
            crate::airgap::proxy::PaneProxy::spawn(effective_allow_patterns(&state), None, |_| {})
                .await
                .unwrap(),
        );
        state.proxies.lock().unwrap().insert("pty-1".to_string(), proxy.clone());

        assert_eq!(
            connect_status(proxy.port(), echo_port).await,
            403,
            "not yet reapplied — the live proxy must still block it"
        );

        // What this file's `ws_sync` now does, once folders_synced flips true.
        state.airgap.reapply_repo_consents(|p| resolve(p));
        recompile_all_proxies(&state);

        assert!(effective_allow_patterns(&state).contains(&"127.0.0.1".to_string()));
        assert_eq!(
            connect_status(proxy.port(), echo_port).await,
            200,
            "reapplied — the SAME already-live proxy must now allow it"
        );
    }

    async fn spawn_test_echo_server() -> (u16, tokio::task::AbortHandle) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind echo server");
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(b"hi").await;
                });
            }
        });
        (port, task.abort_handle())
    }

    /// Sends a raw CONNECT through the pane proxy at `proxy_port` for
    /// `127.0.0.1:<upstream_port>` and returns just the status code.
    async fn connect_status(proxy_port: u16, upstream_port: u16) -> u16 {
        let mut stream = TcpStream::connect(("127.0.0.1", proxy_port)).await.expect("connect to proxy");
        let req = format!("CONNECT 127.0.0.1:{upstream_port} HTTP/1.1\r\nHost: x\r\n\r\n");
        stream.write_all(req.as_bytes()).await.expect("write CONNECT");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).await.expect("read CONNECT response");
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&head).split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }
}
