//! Fire-and-forget state syncs from the renderer. `panes_sync` (pane list,
//! for the conductor) and `ws_sync` (open workspace folders, for
//! `crate::confine`'s root set) are both single-shot "sync renderer state
//! into main" commands with no dedicated Electron module of their own
//! (`src/main/index.js` handles both inline) — grouped here together rather
//! than getting one file each, since the command-surface brief did not list
//! a separate `ws` domain file.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tauri::State;

use crate::{lock_gate, state::AppState};

/// Mirrors `src/main/conductor.js`'s module-level `let panes = []` — the
/// renderer's pane snapshot (`[{id, title}, ...]`), synced fire-and-forget
/// on every pane open/close/rename via `ipcMain.on('panes:sync', (e, list)
/// => conductor.setPanes(list))`.
///
/// `conductor.rs` (a different domain file — `ipc::conductor`, not this
/// one) has no real state yet: its `conductor_allow_run`/
/// `conductor_allow_read` commands are still stubs. `AppState` is out of
/// this slice's scope to extend for a field only this command would use
/// (see this slice's task notes), so the synced list is retained locally
/// here instead, behind the same file that owns the command. Whichever
/// slice ports `conductor.js` for real should fold this into that module's
/// own state and delete this local copy.
static PANES: OnceLock<Mutex<Vec<serde_json::Value>>> = OnceLock::new();

/// Mirrors `conductor.js`'s `setPanes(list)`: `panes = Array.isArray(list)
/// ? list : []` — anything but a JSON array collapses to empty, same as
/// the JS's `Array.isArray` guard. Kept as untyped `serde_json::Value`
/// entries (rather than a typed `{id, title}` struct) because nothing
/// reads this list yet either side of the port; typing it is whichever
/// slice ports `conductor.js` for real.
#[tauri::command]
pub async fn panes_sync(
    state: State<'_, AppState>,
    list: serde_json::Value,
) -> Result<serde_json::Value, String> {
    lock_gate::guard(&state, "panes:sync")?;
    let items = match list {
        serde_json::Value::Array(items) => items,
        _ => Vec::new(),
    };
    *PANES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("panes_sync: PANES lock poisoned") = items;
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
/// NOT ported: the JS handler's trailing `airgap.reapplyRepoConsents()`
/// call. `airgap.rs` has no real consent state yet (Phase 3/4 work, a
/// different slice) — there is nothing to reapply.
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
    Ok(serde_json::json!({}))
}
