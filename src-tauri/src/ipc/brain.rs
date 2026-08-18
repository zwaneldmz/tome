//! Brain (notes vault) commands: open/close/index/read/write/delete plus
//! core-info and promote. Ports `src/main/brain.js` (domain logic lives in
//! `crate::brain`, see that module's doc comment) + `index.js`'s
//! `brain:*` `ipcMain.handle` bodies (~878-890) — this file's job is just
//! the wire-shape translation: `lock_gate::guard` first (every command,
//! matching the JS original's per-handler gate), resolve whatever
//! `AppHandle`/`State` inputs the domain call needs, run the (synchronous,
//! disk-touching) domain call on Tokio's blocking pool via
//! `spawn_blocking` — same discipline `ipc::pty`'s `store::get` call sites
//! already use — then shape the `Result` into the exact `Value` the
//! renderer expects.
//!
//! Vaults live outside the app config dir (`~/Tome/Brains`), so unlike
//! `brain_core_info`/`brain_promote` (which resolve the `core-vault` store
//! key via `app.path().app_data_dir()`), `brain_open`/`_close`/`_index`/
//! `_read`/`_write`/`_delete` need no `app_data_dir` at all.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::{brain, lock_gate, state::AppState};

/// `brain:open` (`{ ws }`) -> `{ root, index }`.
#[tauri::command]
pub async fn brain_open(
    app: AppHandle,
    state: State<'_, AppState>,
    ws: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:open")?;
    tokio::task::spawn_blocking(move || {
        let (root, index) = brain::open(&app, &ws)?;
        Ok::<Value, String>(json!({
            "root": root.to_string_lossy().into_owned(),
            "index": index,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `brain:close` (`{ ws }`). The JS handler's `brain.close(ws)` returns
/// `undefined`; nothing in the renderer reads `tome.brain.close`'s
/// resolved value (`panels/brain.js`'s `dispose()` calls it without
/// awaiting/using the result), so this returns `null` — the same
/// undefined-over-IPC convention already used elsewhere in this crate
/// (for example `ipc::fs::fs_write_file`).
#[tauri::command]
pub async fn brain_close(state: State<'_, AppState>, ws: String) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:close")?;
    tokio::task::spawn_blocking(move || brain::close(&ws))
        .await
        .map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

/// `brain:index` (`{ ws }`) -> the `Index` object directly (`{ root, notes,
/// backlinks }`).
#[tauri::command]
pub async fn brain_index(
    app: AppHandle,
    state: State<'_, AppState>,
    ws: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:index")?;
    let index = tokio::task::spawn_blocking(move || brain::get_index(&app, &ws))
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&index).expect("Index always serializes"))
}

/// `brain:read` (`{ ws, rel }`) -> the note's raw text content.
#[tauri::command]
pub async fn brain_read(
    state: State<'_, AppState>,
    ws: String,
    rel: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:read")?;
    let content = tokio::task::spawn_blocking(move || brain::read_note(&ws, &rel))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Value::String(content))
}

/// `brain:write` (`{ ws, rel, content, exclusive }`) -> `{ ok: true }` or
/// `{ exists: true }` (only reachable when `exclusive` is true and the
/// target already exists). `exclusive` is omitted by `panels/brain.js`'s
/// `save()`/`markPromoted()` call sites (JSON drops the `undefined`
/// property), so this defaults it to `false`, matching the JS original's
/// falsy-when-omitted behavior.
#[tauri::command]
pub async fn brain_write(
    state: State<'_, AppState>,
    ws: String,
    rel: String,
    content: String,
    exclusive: Option<bool>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:write")?;
    let exclusive = exclusive.unwrap_or(false);
    let outcome =
        tokio::task::spawn_blocking(move || brain::write_note(&ws, &rel, &content, exclusive))
            .await
            .map_err(|e| e.to_string())??;
    Ok(match outcome {
        brain::WriteOutcome::Ok => json!({"ok": true}),
        brain::WriteOutcome::Exists => json!({"exists": true}),
    })
}

/// `brain:delete` (`{ ws, rel }`) -> `{ ok: true }` (or an `Err` for a
/// vault-escaping path / the protected `AGENTS.md`).
#[tauri::command]
pub async fn brain_delete(
    state: State<'_, AppState>,
    ws: String,
    rel: String,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:delete")?;
    tokio::task::spawn_blocking(move || brain::delete_note(&ws, &rel))
        .await
        .map_err(|e| e.to_string())??;
    Ok(json!({"ok": true}))
}

/// `brain:coreInfo` (no args) -> `{ configured, root, folders }`. Resolves
/// the `core-vault` store key itself (`readStore('core-vault')` in the JS
/// original) via `crate::store::get` — reused, not duplicated, per this
/// slice's brief.
#[tauri::command]
pub async fn brain_core_info(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:coreInfo")?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let info = tokio::task::spawn_blocking(move || {
        let core_vault = crate::store::get(&dir, "core-vault", locked);
        let root = core_vault.as_str().map(|s| s.to_string());
        brain::core_info(root.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&info).expect("CoreInfo always serializes"))
}

/// `brain:promote` (`{ ws, rel, folder, overwrite, rename }`) -> `{ ok:
/// true, rel }` or `{ collision: true }`. Same `core-vault` resolution as
/// `brain_core_info`.
#[tauri::command]
pub async fn brain_promote(
    app: AppHandle,
    state: State<'_, AppState>,
    ws: String,
    rel: String,
    folder: Option<String>,
    overwrite: Option<bool>,
    rename: Option<bool>,
) -> Result<Value, String> {
    lock_gate::guard(&state, "brain:promote")?;
    let locked = *state.locked.read().expect("AppState.locked lock poisoned");
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let overwrite = overwrite.unwrap_or(false);
    let rename = rename.unwrap_or(false);
    let outcome = tokio::task::spawn_blocking(move || {
        let core_vault = crate::store::get(&dir, "core-vault", locked);
        let core_root = core_vault.as_str().map(|s| s.to_string());
        brain::promote(
            core_root.as_deref(),
            &ws,
            &rel,
            folder.as_deref(),
            overwrite,
            rename,
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(match outcome {
        brain::PromoteOutcome::Ok { rel } => json!({"ok": true, "rel": rel}),
        brain::PromoteOutcome::Collision => json!({"collision": true}),
    })
}
